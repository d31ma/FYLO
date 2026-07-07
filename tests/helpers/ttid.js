// Synchronous in-process TTID for tests.
//
// FYLO drives the `ttid` binary through an async subprocess shim at runtime, but
// tests only need to mint valid ids for fixtures synchronously. TTID's operations
// are pure base-36 timestamp math, so this small self-contained implementation
// mirrors the binary's format exactly (same constants and patterns), keeping ids
// interoperable. Flattened from the TTID source (constants + generator +
// validator + time).

const PRECISION = 10_000
const BASE = 36
const PLACEHOLDER = 'X'
const MIN_TIMESTAMP_MS = 1_577_836_800_000
const MAX_TIMESTAMP_MS = 7_258_118_400_000
const TTID_PATTERN = /^[A-Z0-9]{11}(-[A-Z0-9]{1,11}){0,2}$/i
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

/** @returns {number} */
function timeNow() {
    return (performance.now() + performance.timeOrigin) * PRECISION
}

/**
 * @param {string} _id
 * @returns {{ createdAt: number, updatedAt?: number, deletedAt?: number }}
 */
function decodeTime(_id) {
    if (!TTID_PATTERN.test(_id)) throw new Error('Invalid Format!')
    const [created, updated, deleted] = _id.split('-')
    /** @param {string} timeCode @returns {number} */
    const toMs = (timeCode) => {
        const ms = Number((parseInt(timeCode, BASE) / PRECISION).toFixed(0))
        if (!isFinite(ms) || ms < MIN_TIMESTAMP_MS || ms > MAX_TIMESTAMP_MS) {
            throw new Error('Invalid timestamp encoding')
        }
        return ms
    }
    /** @type {{ createdAt: number, updatedAt?: number, deletedAt?: number }} */
    const timestamps = { createdAt: toMs(created) }
    if (updated && updated !== PLACEHOLDER) timestamps.updatedAt = toMs(updated)
    if (deleted) timestamps.deletedAt = toMs(deleted)
    return timestamps
}

/**
 * @param {string} _id
 * @returns {Date | null}
 */
function isTTID(_id) {
    if (!_id || _id.length > 36) return null
    if (!TTID_PATTERN.test(_id)) return null
    try {
        return new Date(decodeTime(_id).createdAt)
    } catch {
        return null
    }
}

/**
 * @param {string} [_id]
 * @param {boolean} [del]
 * @returns {string}
 */
function generate(_id, del = false) {
    if (_id && isTTID(_id) && _id.split('-').length === 3) {
        throw new Error('This identifier can no longer be modified')
    }
    const time = timeNow()
    if (_id && isTTID(_id) && del) {
        const [created, updated] = _id.split('-')
        const deleted = time.toString(BASE)
        return `${created}-${updated ?? PLACEHOLDER}-${deleted}`.toUpperCase()
    }
    if (_id && isTTID(_id)) {
        const [created] = _id.split('-')
        return `${created}-${time.toString(BASE)}`.toUpperCase()
    }
    if (_id && !isTTID(_id)) throw new Error('Invalid TTID!')
    return time.toString(BASE).toUpperCase()
}

export default class TTID {
    /** @param {string} _id @returns {Date | null} */
    static isTTID(_id) {
        return isTTID(_id)
    }
    /** @param {string} _id @returns {RegExpMatchArray | null} */
    static isUUID(_id) {
        return _id.match(UUID_PATTERN)
    }
    /** @param {string} [_id] @param {boolean} [del] @returns {string} */
    static generate(_id, del = false) {
        return generate(_id, del)
    }
    /** @param {string} _id @returns {{ createdAt: number, updatedAt?: number, deletedAt?: number }} */
    static decodeTime(_id) {
        return decodeTime(_id)
    }
}
