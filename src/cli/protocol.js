export const MACHINE_PROTOCOL_VERSION = 1
export const DEFAULT_MAX_REQUEST_FRAME_BYTES = 1024 * 1024
export const DEFAULT_MAX_RESPONSE_FRAME_BYTES = 8 * 1024 * 1024
export const MIN_REQUEST_FRAME_BYTES = 256
export const MIN_RESPONSE_FRAME_BYTES = 1024
export const MAX_CONFIGURED_FRAME_BYTES = 64 * 1024 * 1024

const encoder = new TextEncoder()
const fatalDecoder = new TextDecoder('utf-8', { fatal: true })

export class MachineFrameError extends Error {
    /** @type {string} */
    code

    /** @param {string} code @param {string} message */
    constructor(code, message) {
        super(message)
        this.name = 'MachineFrameError'
        this.code = code
    }
}

/** @param {unknown} value @param {string} name @param {number} minimum */
function boundedFrameLimit(value, name, minimum) {
    if (
        !Number.isSafeInteger(value) ||
        Number(value) < minimum ||
        Number(value) > MAX_CONFIGURED_FRAME_BYTES
    ) {
        throw new RangeError(
            `${name} must be an integer between ${minimum} and ${MAX_CONFIGURED_FRAME_BYTES}`
        )
    }
    return Number(value)
}

/**
 * @param {{ maxRequestBytes?: number, maxResponseBytes?: number }=} limits
 */
export function normalizeMachineFrameLimits(limits = {}) {
    return {
        maxRequestBytes: boundedFrameLimit(
            limits.maxRequestBytes ?? DEFAULT_MAX_REQUEST_FRAME_BYTES,
            'maxRequestBytes',
            MIN_REQUEST_FRAME_BYTES
        ),
        maxResponseBytes: boundedFrameLimit(
            limits.maxResponseBytes ?? DEFAULT_MAX_RESPONSE_FRAME_BYTES,
            'maxResponseBytes',
            MIN_RESPONSE_FRAME_BYTES
        )
    }
}

/**
 * Fixed-capacity NDJSON decoder. The LF delimiter is not part of a frame and
 * does not count toward `maxBytes`. Once a frame crosses the limit, its
 * remaining bytes are discarded until LF without allocating more memory.
 */
export class BoundedNdjsonDecoder {
    /** @param {number} maxBytes */
    constructor(maxBytes) {
        this.maxBytes = maxBytes
        this.buffer = new Uint8Array(maxBytes)
        this.length = 0
        this.oversized = false
    }

    /** @param {Uint8Array | string} chunk */
    *push(chunk) {
        const bytes = typeof chunk === 'string' ? encoder.encode(chunk) : chunk
        let start = 0
        while (start < bytes.byteLength) {
            const newline = bytes.indexOf(0x0a, start)
            const end = newline === -1 ? bytes.byteLength : newline
            this.#append(bytes.subarray(start, end))
            if (newline === -1) return

            if (this.oversized) {
                yield {
                    error: new MachineFrameError(
                        'EFRAME_REQUEST_TOO_LARGE',
                        `Machine request frame exceeds ${this.maxBytes} bytes`
                    )
                }
            } else {
                yield { frame: this.buffer.slice(0, this.length) }
            }
            this.length = 0
            this.oversized = false
            start = newline + 1
        }
    }

    *finish() {
        if (this.oversized) {
            yield {
                error: new MachineFrameError(
                    'EFRAME_REQUEST_TOO_LARGE',
                    `Machine request frame exceeds ${this.maxBytes} bytes`
                )
            }
        } else if (this.length > 0) {
            yield {
                error: new MachineFrameError(
                    'EFRAME_TRUNCATED',
                    'Machine request stream ended before the newline delimiter'
                )
            }
        }
        this.length = 0
        this.oversized = false
    }

    /** @param {Uint8Array} bytes */
    #append(bytes) {
        if (this.oversized || bytes.byteLength === 0) return
        if (bytes.byteLength > this.maxBytes - this.length) {
            this.length = 0
            this.oversized = true
            return
        }
        this.buffer.set(bytes, this.length)
        this.length += bytes.byteLength
    }
}

/** @param {string} text */
function parseJsonWithoutDuplicateKeys(text) {
    let index = 0
    const maximumDepth = 128

    /** @returns {never} */
    const fail = () => {
        throw new MachineFrameError('EFRAME_JSON', 'Machine request frame is not valid JSON')
    }
    const skipWhitespace = () => {
        while (
            text[index] === ' ' ||
            text[index] === '\t' ||
            text[index] === '\r' ||
            text[index] === '\n'
        ) {
            index++
        }
    }
    const parseString = () => {
        if (text[index] !== '"') fail()
        const start = index++
        while (index < text.length) {
            const code = text.charCodeAt(index)
            if (code === 0x22) {
                index++
                try {
                    return JSON.parse(text.slice(start, index))
                } catch {
                    fail()
                }
            }
            if (code < 0x20) fail()
            if (code === 0x5c) {
                index++
                const escaped = text[index]
                if (escaped === 'u') {
                    const hex = text.slice(index + 1, index + 5)
                    if (!/^[0-9a-fA-F]{4}$/.test(hex)) fail()
                    index += 5
                    continue
                }
                if (!escaped || !'"\\/bfnrt'.includes(escaped)) fail()
            }
            index++
        }
        fail()
    }
    const parseNumber = () => {
        let cursor = index
        if (text[cursor] === '-') cursor++
        if (text[cursor] === '0') {
            cursor++
        } else if (text[cursor] >= '1' && text[cursor] <= '9') {
            while (text[cursor] >= '0' && text[cursor] <= '9') cursor++
        } else {
            fail()
        }
        if (text[cursor] === '.') {
            cursor++
            if (text[cursor] < '0' || text[cursor] > '9') fail()
            while (text[cursor] >= '0' && text[cursor] <= '9') cursor++
        }
        if (text[cursor] === 'e' || text[cursor] === 'E') {
            cursor++
            if (text[cursor] === '+' || text[cursor] === '-') cursor++
            if (text[cursor] < '0' || text[cursor] > '9') fail()
            while (text[cursor] >= '0' && text[cursor] <= '9') cursor++
        }
        index = cursor
    }
    /** @param {number} depth */
    const parseValue = (depth) => {
        if (depth > maximumDepth) {
            throw new MachineFrameError(
                'EFRAME_JSON',
                `Machine request JSON nesting exceeds ${maximumDepth}`
            )
        }
        skipWhitespace()
        const token = text[index]
        if (token === '"') {
            parseString()
            return
        }
        if (token === '{') {
            index++
            skipWhitespace()
            const keys = new Set()
            if (text[index] === '}') {
                index++
                return
            }
            while (true) {
                skipWhitespace()
                const key = parseString()
                if (keys.has(key)) {
                    throw new MachineFrameError(
                        'EFRAME_DUPLICATE_KEY',
                        `Machine request JSON contains duplicate object key "${key}"`
                    )
                }
                keys.add(key)
                skipWhitespace()
                if (text[index++] !== ':') fail()
                parseValue(depth + 1)
                skipWhitespace()
                const separator = text[index++]
                if (separator === '}') return
                if (separator !== ',') fail()
            }
        }
        if (token === '[') {
            index++
            skipWhitespace()
            if (text[index] === ']') {
                index++
                return
            }
            while (true) {
                parseValue(depth + 1)
                skipWhitespace()
                const separator = text[index++]
                if (separator === ']') return
                if (separator !== ',') fail()
            }
        }
        if (text.startsWith('true', index)) {
            index += 4
            return
        }
        if (text.startsWith('false', index)) {
            index += 5
            return
        }
        if (text.startsWith('null', index)) {
            index += 4
            return
        }
        parseNumber()
    }

    parseValue(0)
    skipWhitespace()
    if (index !== text.length) fail()
    try {
        return JSON.parse(text)
    } catch {
        fail()
    }
}

/**
 * @param {Uint8Array} frame
 * @returns {unknown | null} null for an ASCII JSON-whitespace-only frame
 */
export function parseMachineFrame(frame) {
    let text
    try {
        text = fatalDecoder.decode(frame)
    } catch {
        throw new MachineFrameError('EFRAME_UTF8', 'Machine request frame is not valid UTF-8')
    }
    if (/^[\x20\t\r\n]*$/.test(text)) return null
    return parseJsonWithoutDuplicateKeys(text)
}

/** @param {unknown} value */
export function encodedJsonBytes(value) {
    return encoder.encode(JSON.stringify(value))
}
