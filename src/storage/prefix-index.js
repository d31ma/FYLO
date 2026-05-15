import path from 'node:path'
import { mkdir, open, rm, stat } from 'node:fs/promises'
import { Cipher } from '../security/cipher.js'
import { validateDocId } from '../core/doc-id.js'
import { writeDurable } from './durable.js'
import { stringifyStoredValue } from './value-codec.js'

/**
 * @typedef {import('../types/vendor.js').TTID} TTID
 * @typedef {import('../query/types.js').Operand} Operand
 */

const MAX_STRING_PREFIX_BYTES = 180
const MAX_NGRAM_SOURCE_CHARS = 512
const NGRAM_SIZE = 3
const UINT64_MAX = (1n << 64n) - 1n
const SIGN_MASK = 1n << 63n

/**
 * @typedef {object} S3ClientIndexOptions
 * @property {string=} accessKeyId
 * @property {string=} secretAccessKey
 * @property {string=} sessionToken
 * @property {string=} endpoint
 * @property {string=} region
 */

const LOCAL_FS_FORMAT = 'fylo.local-fs.index.v1'
const LOCAL_FS_MANIFEST = 'manifest.json'
const LOCAL_FS_SNAPSHOT = 'keys.snapshot'
const LOCAL_FS_WAL = 'keys.wal'
const LOCAL_FS_WAL_COMPACT_BYTES = 1_048_576

/**
 * @param {string} value
 * @returns {string}
 */
function encodeSegment(value) {
    return encodeURIComponent(value)
}

/**
 * @param {string} value
 * @returns {string}
 */
function decodeSegment(value) {
    return decodeURIComponent(value)
}

/**
 * @param {string} value
 * @returns {string}
 */
function hashValue(value) {
    return new Bun.CryptoHasher('sha256').update(value).digest('hex')
}

/**
 * @param {string} value
 * @returns {string}
 */
function lookupToken(value) {
    const encoded = encodeSegment(value)
    if (new TextEncoder().encode(encoded).byteLength <= MAX_STRING_PREFIX_BYTES) {
        return encoded
    }
    return `h_${hashValue(value)}`
}

/**
 * @param {number} value
 * @returns {string}
 */
function sortableFloat64(value) {
    if (!Number.isFinite(value)) return ''
    const buffer = new ArrayBuffer(8)
    const view = new DataView(buffer)
    view.setFloat64(0, value, false)
    const bits = view.getBigUint64(0, false)
    const sortable = bits & SIGN_MASK ? ~bits & UINT64_MAX : bits ^ SIGN_MASK
    return sortable.toString(16).padStart(16, '0')
}

/**
 * @param {string} sortable
 * @returns {string}
 */
function reverseSortable(sortable) {
    const value = BigInt(`0x${sortable}`)
    return (UINT64_MAX - value).toString(16).padStart(16, '0')
}

/**
 * @param {unknown} value
 * @returns {number | null}
 */
function numericValue(value) {
    const numeric = typeof value === 'number' ? value : Number(value)
    return Number.isFinite(numeric) ? numeric : null
}

/**
 * @param {string} value
 * @returns {string}
 */
function reverseString(value) {
    return Array.from(value).reverse().join('')
}

/**
 * @param {string[]} names
 * @returns {string | undefined}
 */
function envValue(names) {
    for (const name of names) {
        const value = process.env[name]
        if (value) return value
    }
    return undefined
}

/**
 * @param {S3ClientIndexOptions} options
 * @returns {S3ClientIndexOptions}
 */
function resolveS3Options(options) {
    return {
        accessKeyId:
            options.accessKeyId ?? envValue(['AWS_ACCESS_KEY_ID', 'FYLO_S3_ACCESS_KEY_ID']),
        secretAccessKey:
            options.secretAccessKey ??
            envValue(['AWS_SECRET_ACCESS_KEY', 'FYLO_S3_SECRET_ACCESS_KEY']),
        sessionToken:
            options.sessionToken ?? envValue(['AWS_SESSION_TOKEN', 'FYLO_S3_SESSION_TOKEN']),
        endpoint:
            options.endpoint ??
            envValue(['AWS_ENDPOINT_URL_S3', 'AWS_ENDPOINT_URL', 'FYLO_S3_ENDPOINT']),
        region: options.region ?? envValue(['AWS_REGION', 'AWS_DEFAULT_REGION', 'FYLO_S3_REGION'])
    }
}

/**
 * @param {string} value
 * @returns {string[]}
 */
function trigrams(value) {
    const source = value.slice(0, MAX_NGRAM_SOURCE_CHARS)
    if (Array.from(source).length < NGRAM_SIZE) return []
    const grams = new Set()
    const chars = Array.from(source)
    for (let i = 0; i <= chars.length - NGRAM_SIZE; i++) {
        grams.add(chars.slice(i, i + NGRAM_SIZE).join(''))
    }
    return Array.from(grams)
}

/**
 * @param {string} fieldPath
 * @returns {string}
 */
function encodeFieldPath(fieldPath) {
    return fieldPath.split('/').map(encodeSegment).join('/')
}

/**
 * @param {string} prefix
 * @param {string} key
 * @returns {TTID}
 */
function docIdFromKey(prefix, key) {
    const suffix = key.slice(prefix.length)
    const segments = suffix.split('/')
    const docId = decodeSegment(segments.at(-1) ?? '')
    validateDocId(docId)
    return docId
}

/**
 * @param {string} collection
 * @param {string} fieldPath
 * @param {unknown} value
 * @returns {Promise<string>}
 */
async function queryLookupValue(collection, fieldPath, value) {
    let rawValue = stringifyStoredValue(value)
    if (Cipher.isConfigured() && Cipher.isEncryptedField(collection, fieldPath)) {
        rawValue = await Cipher.blindIndex(rawValue)
    }
    return lookupToken(rawValue)
}

export class PrefixIndexCodec {
    /**
     * @param {string} fieldPath
     * @param {string} kind
     * @param {string} value
     * @param {TTID} docId
     * @returns {string}
     */
    static key(fieldPath, kind, value, docId) {
        validateDocId(docId)
        return [encodeFieldPath(fieldPath), kind, value, encodeSegment(docId)].join('/')
    }

    /**
     * @param {string} fieldPath
     * @param {string} kind
     * @param {string} [valuePrefix]
     * @returns {string}
     */
    static prefix(fieldPath, kind, valuePrefix = '') {
        return [encodeFieldPath(fieldPath), kind, valuePrefix].join('/')
    }

    /**
     * @param {string} collection
     * @param {TTID} docId
     * @param {Record<string, any>} data
     * @returns {Promise<string[]>}
     */
    static async entriesForDocument(collection, docId, data) {
        /** @type {string[]} */
        const entries = []
        /**
         * @param {string} fieldPath
         * @param {unknown} raw
         * @returns {Promise<void>}
         */
        const addValue = async (fieldPath, raw) => {
            let value = stringifyStoredValue(raw)
            const encrypted =
                Cipher.isConfigured() && Cipher.isEncryptedField(collection, fieldPath)
            if (encrypted) value = await Cipher.blindIndex(value)

            entries.push(this.key(fieldPath, 'eq', lookupToken(value), docId))

            const numeric = numericValue(raw)
            const sortable = numeric === null ? '' : sortableFloat64(numeric)
            if (sortable) {
                entries.push(this.key(fieldPath, 'n', sortable, docId))
                entries.push(this.key(fieldPath, 'nr', reverseSortable(sortable), docId))
            }

            if (typeof raw !== 'string' || encrypted) return
            const encoded = encodeSegment(value)
            if (new TextEncoder().encode(encoded).byteLength > MAX_STRING_PREFIX_BYTES) return
            entries.push(this.key(fieldPath, 'f', encoded, docId))
            entries.push(this.key(fieldPath, 'r', encodeSegment(reverseString(value)), docId))
            for (const gram of trigrams(value)) {
                entries.push(this.key(fieldPath, 'g3', encodeSegment(gram), docId))
            }
        }

        /**
         * @param {Record<string, any>} target
         * @param {string=} parentField
         * @returns {Promise<void>}
         */
        const walk = async (target, parentField) => {
            for (const field in target) {
                const fieldPath = parentField ? `${parentField}/${field}` : field
                const value = target[field]
                if (value && typeof value === 'object' && !Array.isArray(value)) {
                    await walk(value, fieldPath)
                    continue
                }
                if (Array.isArray(value)) {
                    for (const item of value) {
                        if (item && typeof item === 'object') {
                            throw new Error('Cannot index an array of objects')
                        }
                        await addValue(fieldPath, item)
                    }
                    continue
                }
                await addValue(fieldPath, value)
            }
        }

        await walk(data)
        return Array.from(new Set(entries))
    }

    /**
     * @param {string} collection
     * @param {string} fieldPath
     * @param {Operand} operand
     * @returns {Promise<Array<{ kind: string, valuePrefix: string, range?: { op: '$gt' | '$gte' | '$lt' | '$lte', value: string } }> | null>}
     */
    static async queryPrefixes(collection, fieldPath, operand) {
        if (operand.$eq !== undefined) {
            return [
                {
                    kind: 'eq',
                    valuePrefix: `${await queryLookupValue(collection, fieldPath, operand.$eq)}/`
                }
            ]
        }
        if (operand.$contains !== undefined) {
            return [
                {
                    kind: 'eq',
                    valuePrefix: `${await queryLookupValue(
                        collection,
                        fieldPath,
                        operand.$contains
                    )}/`
                }
            ]
        }
        if (operand.$like !== undefined) {
            const pattern = operand.$like
            const wildcardCount = (pattern.match(/%/g) ?? []).length
            if (wildcardCount === 0) {
                return [
                    {
                        kind: 'eq',
                        valuePrefix: `${await queryLookupValue(collection, fieldPath, pattern)}/`
                    }
                ]
            }
            if (wildcardCount === 1 && pattern.endsWith('%')) {
                return [{ kind: 'f', valuePrefix: encodeSegment(pattern.slice(0, -1)) }]
            }
            if (wildcardCount === 1 && pattern.startsWith('%')) {
                return [{ kind: 'r', valuePrefix: encodeSegment(reverseString(pattern.slice(1))) }]
            }
            if (
                wildcardCount === 2 &&
                pattern.startsWith('%') &&
                pattern.endsWith('%') &&
                pattern.length > 2
            ) {
                const needle = pattern.slice(1, -1)
                if (Array.from(needle).length >= NGRAM_SIZE) {
                    return [{ kind: 'g3', valuePrefix: `${encodeSegment(needle.slice(0, 3))}/` }]
                }
            }
            return null
        }
        const rangeEntries = []
        for (const op of /** @type {const} */ (['$gt', '$gte', '$lt', '$lte'])) {
            const raw = operand[op]
            if (raw === undefined) continue
            const numeric = numericValue(raw)
            const sortable = numeric === null ? '' : sortableFloat64(numeric)
            if (!sortable) return null
            if (op === '$gt' || op === '$gte') {
                rangeEntries.push({ kind: 'n', valuePrefix: '', range: { op, value: sortable } })
            } else {
                rangeEntries.push({
                    kind: 'nr',
                    valuePrefix: '',
                    range: { op, value: reverseSortable(sortable) }
                })
            }
        }
        return rangeEntries.length ? rangeEntries : null
    }

    /**
     * @param {string} key
     * @returns {string}
     */
    static rangeValueFromKey(key) {
        const segments = key.split('/')
        return segments.at(-2) ?? ''
    }
}

/**
 * @param {string} text
 * @returns {string[]}
 */
function lines(text) {
    if (!text) return []
    const complete = text.endsWith('\n') ? text : text.slice(0, text.lastIndexOf('\n') + 1)
    return complete.split('\n').filter(Boolean)
}

/**
 * @param {string} target
 * @returns {Promise<string>}
 */
async function readTextIfExists(target) {
    try {
        return await Bun.file(target).text()
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code === 'ENOENT') return ''
        throw err
    }
}

/**
 * @param {string} target
 * @param {string} data
 * @returns {Promise<void>}
 */
async function appendDurable(target, data) {
    await mkdir(path.dirname(target), { recursive: true })
    const handle = await open(target, 'a')
    try {
        await handle.writeFile(data)
        await handle.sync()
    } finally {
        await handle.close()
    }
}

/**
 * @param {string} target
 * @param {string} data
 * @returns {Promise<void>}
 */
async function writeIfMissingDurable(target, data) {
    await mkdir(path.dirname(target), { recursive: true })
    let handle
    try {
        handle = await open(target, 'wx')
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code === 'EEXIST') return
        throw err
    }
    try {
        await handle.writeFile(data)
        await handle.sync()
    } finally {
        await handle.close()
    }
}

/**
 * @param {string[]} keys
 * @returns {string}
 */
function serializeSnapshot(keys) {
    return keys.length ? `${keys.join('\n')}\n` : ''
}

/**
 * @param {{ op: '+' | '-', key: string }[]} mutations
 * @returns {string}
 */
function serializeWal(mutations) {
    return mutations.map((mutation) => `${mutation.op}\t${mutation.key}\n`).join('')
}

export class LocalFsPrefixIndexStore {
    /** @type {(collection: string) => string} */
    rootForCollection

    /**
     * @param {(collection: string) => string} rootForCollection
     */
    constructor(rootForCollection) {
        this.rootForCollection = rootForCollection
    }

    /**
     * @param {string} collection
     * @returns {string}
     */
    root(collection) {
        return path.join(this.rootForCollection(collection), '.fylo', 'local-fs')
    }

    /**
     * @param {string} collection
     * @returns {string}
     */
    manifestPath(collection) {
        return path.join(this.root(collection), LOCAL_FS_MANIFEST)
    }

    /**
     * @param {string} collection
     * @returns {string}
     */
    snapshotPath(collection) {
        return path.join(this.root(collection), LOCAL_FS_SNAPSHOT)
    }

    /**
     * @param {string} collection
     * @returns {string}
     */
    walPath(collection) {
        return path.join(this.root(collection), LOCAL_FS_WAL)
    }

    /**
     * Returns a memory-mapped view of the sorted keys file.
     * Uses Bun.mmap so only pages touched by binary search are faulted in
     * (OS page cache handles the rest — no full-file load into JS heap).
     *
     * @param {string} collection
     * @returns Uint8Array
     */
    #snapshotBuffer(collection) {
        try {
            return Bun.mmap(this.snapshotPath(collection))
        } catch {
            return new Uint8Array(0)
        }
    }

    /**
     * Binary search for the byte offset of the first key >= prefix.
     * Operates directly on the mmap'd buffer — O(log n) key comparisons.
     *
     * @param {Uint8Array} buf
     * @param {string} prefix
     * @returns {number} byte offset of the start of the matching line
     */
    #findGE(buf, prefix) {
        if (buf.length === 0) return 0
        const decoder = new TextDecoder()
        let lo = 0
        let hi = buf.length
        while (lo < hi) {
            const mid = Math.floor((lo + hi) / 2)
            let start = mid
            while (start > 0 && buf[start - 1] !== 0x0a) start--
            let end = mid
            while (end < buf.length && buf[end] !== 0x0a) end++
            const key = decoder.decode(buf.subarray(start, end))
            if (key < prefix) lo = end + 1
            else hi = start
        }
        while (lo > 0 && lo < buf.length && buf[lo - 1] !== 0x0a) lo--
        return lo
    }

    /** @param {string} collection @returns {Promise<void>} */
    async ensureCollection(collection) {
        const root = this.root(collection)
        await mkdir(root, { recursive: true })
        await writeIfMissingDurable(
            this.manifestPath(collection),
            `${JSON.stringify({ format: LOCAL_FS_FORMAT, createdAt: Date.now() })}\n`
        )
        await writeIfMissingDurable(this.snapshotPath(collection), '')
        await writeIfMissingDurable(this.walPath(collection), '')
    }

    /** @param {string} collection @returns {Promise<void>} */
    async resetCollection(collection) {
        await rm(this.root(collection), { recursive: true, force: true })
        await this.ensureCollection(collection)
    }

    /** @param {string} collection @param {string} key @returns {Promise<void>} */
    async putKey(collection, key) {
        await this.appendMutations(collection, [{ op: '+', key }])
    }

    /** @param {string} collection @param {string} key @returns {Promise<void>} */
    async deleteKey(collection, key) {
        await this.appendMutations(collection, [{ op: '-', key }])
    }

    /**
     * Loads the full key set (snapshot + WAL) into a JS Set.
     * Used only during compaction — not in the hot query path.
     *
     * @param {string} collection
     * @returns {Promise<Set<string>>}
     */
    async loadKeySet(collection) {
        await this.ensureCollection(collection)
        // Read snapshot efficiently via mmap lines (avoids full string decode)
        const snapBuf = this.#snapshotBuffer(collection)
        const keys = new Set(lines(new TextDecoder().decode(snapBuf)))
        for (const line of lines(await readTextIfExists(this.walPath(collection)))) {
            const op = line[0]
            if (line[1] !== '\t') continue
            const key = line.slice(2)
            if (!key) continue
            if (op === '+') keys.add(key)
            else if (op === '-') keys.delete(key)
        }
        return keys
    }

    /**
     * @param {string} collection
     * @param {{ op: '+' | '-', key: string }[]} mutations
     * @returns {Promise<void>}
     */
    async appendMutations(collection, mutations) {
        if (!mutations.length) return
        await this.ensureCollection(collection)
        await appendDurable(this.walPath(collection), serializeWal(mutations))
        await this.compactIfNeeded(collection)
    }

    /**
     * @param {string} collection
     * @returns {Promise<void>}
     */
    async compactIfNeeded(collection) {
        try {
            const info = await stat(this.walPath(collection))
            if (info.size < LOCAL_FS_WAL_COMPACT_BYTES) return
        } catch {
            return
        }
        await this.compact(collection)
    }

    /**
     * Merges WAL into the sorted snapshot atomically.
     * After compaction any subsequent listKeys() picks up the new
     * file via a fresh mmap (mmap sees the rewritten file on next call).
     *
     * @param {string} collection
     * @returns {Promise<void>}
     */
    async compact(collection) {
        const keys = Array.from(await this.loadKeySet(collection)).sort()
        await writeDurable(this.snapshotPath(collection), serializeSnapshot(keys))
        await writeDurable(this.walPath(collection), '')
    }

    /**
     * Lists index keys matching a prefix using O(log n) binary search
     * on the mmap'd snapshot + O(k) sequential scan of matching keys
     * + WAL merge. Never loads the full key set into JS memory.
     *
     * @param {string} collection
     * @param {string} [prefix]
     * @returns {Promise<string[]>}
     */
    async listKeys(collection, prefix = '') {
        await this.ensureCollection(collection)
        const buf = this.#snapshotBuffer(collection)
        const decoder = new TextDecoder()
        const result = new Set()

        // Phase 1: binary search + range scan on mmap'd snapshot
        let pos = this.#findGE(buf, prefix)
        while (pos < buf.length) {
            let end = pos
            while (end < buf.length && buf[end] !== 0x0a) end++
            const key = decoder.decode(buf.subarray(pos, end))
            if (!key || !key.startsWith(prefix)) break
            result.add(key)
            pos = end + 1
        }

        // Phase 2: apply WAL mutations on top (only for matching prefix)
        for (const line of lines(await readTextIfExists(this.walPath(collection)))) {
            const op = line[0]
            if (line[1] !== '\t') continue
            const key = line.slice(2)
            if (!key || !key.startsWith(prefix)) continue
            if (op === '+') result.add(key)
            else if (op === '-') result.delete(key)
        }

        return Array.from(result).sort()
    }

    /** @param {string} collection @param {TTID} docId @param {Record<string, any>} doc @returns {Promise<void>} */
    async putDocument(collection, docId, doc) {
        const keys = await PrefixIndexCodec.entriesForDocument(collection, docId, doc)
        await this.appendMutations(
            collection,
            keys.map((key) => ({ op: '+', key }))
        )
    }

    /** @param {string} collection @param {TTID} docId @param {Record<string, any>} doc @returns {Promise<void>} */
    async removeDocument(collection, docId, doc) {
        const keys = await PrefixIndexCodec.entriesForDocument(collection, docId, doc)
        await this.appendMutations(
            collection,
            keys.map((key) => ({ op: '-', key }))
        )
    }

    /** @param {string} collection @returns {Promise<number>} */
    async countDocuments(collection) {
        const keys = await this.listKeys(collection)
        const docIds = new Set()
        for (const key of keys) {
            const docId = decodeSegment(key.split('/').at(-1) ?? '')
            try {
                validateDocId(docId)
                docIds.add(docId)
            } catch {}
        }
        return docIds.size
    }

    /**
     * @param {string} collection
     * @param {string} fieldPath
     * @param {Operand} operand
     * @returns {Promise<Set<TTID> | null>}
     */
    async candidateDocIds(collection, fieldPath, operand) {
        const prefixSpecs = await PrefixIndexCodec.queryPrefixes(collection, fieldPath, operand)
        if (prefixSpecs === null) return null
        /** @type {Set<TTID> | null} */
        let candidates = null
        for (const spec of prefixSpecs) {
            const prefix = PrefixIndexCodec.prefix(fieldPath, spec.kind, spec.valuePrefix)
            const keys = await this.listKeys(collection, prefix)
            const next = new Set()
            for (const key of keys) {
                if (spec.range) {
                    const value = PrefixIndexCodec.rangeValueFromKey(key)
                    const inclusive = spec.range.op === '$gte' || spec.range.op === '$lte'
                    if (inclusive ? value < spec.range.value : value <= spec.range.value) {
                        continue
                    }
                }
                next.add(docIdFromKey(prefix, key))
            }
            candidates = intersect(candidates, next)
        }
        return candidates
    }
}

export class BunS3ClientIndexStore {
    /** @type {S3ClientIndexOptions} */
    options
    /**
     * @param {S3ClientIndexOptions} [options]
     */
    constructor(options = {}) {
        this.options = resolveS3Options(options)
    }

    /** @param {string} collection @returns {Bun.S3Client} */
    client(collection) {
        return new Bun.S3Client({
            ...this.options,
            bucket: collection
        })
    }

    /** @param {string} _collection @returns {Promise<void>} */
    async ensureCollection(_collection) {}

    /** @param {string} collection @returns {Promise<void>} */
    async resetCollection(collection) {
        const client = this.client(collection)
        let startAfter
        do {
            const page = await client.list({ prefix: '', startAfter })
            const keys = page.contents?.map((item) => item.key).filter(Boolean) ?? []
            await Promise.all(keys.map((key) => client.delete(key)))
            startAfter = page.isTruncated ? keys.at(-1) : undefined
        } while (startAfter)
    }

    /** @param {string} collection @param {string} key @returns {Promise<void>} */
    async putKey(collection, key) {
        await this.client(collection).write(key, '')
    }

    /** @param {string} collection @param {string} key @returns {Promise<void>} */
    async deleteKey(collection, key) {
        await this.client(collection).delete(key)
    }

    /** @param {string} collection @param {string} prefix @returns {Promise<string[]>} */
    async listKeys(collection, prefix = '') {
        const client = this.client(collection)
        /** @type {string[]} */
        const keys = []
        let startAfter
        do {
            const page = await client.list({ prefix, startAfter })
            const pageKeys = page.contents?.map((item) => item.key).filter(Boolean) ?? []
            keys.push(...pageKeys)
            startAfter = page.isTruncated ? pageKeys.at(-1) : undefined
        } while (startAfter)
        return keys
    }

    /** @param {string} collection @param {TTID} docId @param {Record<string, any>} doc @returns {Promise<void>} */
    async putDocument(collection, docId, doc) {
        const keys = await PrefixIndexCodec.entriesForDocument(collection, docId, doc)
        await Promise.all(keys.map((key) => this.putKey(collection, key)))
    }

    /** @param {string} collection @param {TTID} docId @param {Record<string, any>} doc @returns {Promise<void>} */
    async removeDocument(collection, docId, doc) {
        const keys = await PrefixIndexCodec.entriesForDocument(collection, docId, doc)
        await Promise.all(keys.map((key) => this.deleteKey(collection, key)))
    }

    /** @param {string} collection @returns {Promise<number>} */
    async countDocuments(collection) {
        const keys = await this.listKeys(collection)
        const docIds = new Set()
        for (const key of keys) {
            const docId = decodeSegment(key.split('/').at(-1) ?? '')
            try {
                validateDocId(docId)
                docIds.add(docId)
            } catch {}
        }
        return docIds.size
    }

    /**
     * @param {string} collection
     * @param {string} fieldPath
     * @param {Operand} operand
     * @returns {Promise<Set<TTID> | null>}
     */
    async candidateDocIds(collection, fieldPath, operand) {
        const prefixSpecs = await PrefixIndexCodec.queryPrefixes(collection, fieldPath, operand)
        if (prefixSpecs === null) return null
        /** @type {Set<TTID> | null} */
        let candidates = null
        for (const spec of prefixSpecs) {
            const prefix = PrefixIndexCodec.prefix(fieldPath, spec.kind, spec.valuePrefix)
            const keys = await this.listKeys(collection, prefix)
            const next = new Set()
            for (const key of keys) {
                if (spec.range) {
                    const value = PrefixIndexCodec.rangeValueFromKey(key)
                    const inclusive = spec.range.op === '$gte' || spec.range.op === '$lte'
                    if (inclusive ? value < spec.range.value : value <= spec.range.value) {
                        continue
                    }
                }
                next.add(docIdFromKey(prefix, key))
            }
            candidates = intersect(candidates, next)
        }
        return candidates
    }
}

/**
 * @template T
 * @param {Set<T> | null} current
 * @param {Set<T>} next
 * @returns {Set<T>}
 */
function intersect(current, next) {
    if (current === null) return new Set(next)
    const intersection = new Set()
    for (const value of current) {
        if (next.has(value)) intersection.add(value)
    }
    return intersection
}
