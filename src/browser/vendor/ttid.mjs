// @ts-nocheck
// TTID web client — a self-contained, dependency-free implementation for the
// browser (and any pure-JS runtime).
//
// Unlike the other clients in this folder, the web build does NOT drive the
// `ttid` binary — a browser can't spawn a subprocess. TTID is pure computation
// (base-36 timestamp encoding + validation), so this file reimplements it
// natively. No bundler, no dependencies: drop it in and import.
//
//   import TTID from './ttid.mjs'
//
//   const id = TTID.generate()            // new id, e.g. "4VLN5QUCDP0"
//   const updated = TTID.generate(id)     // advance it
//   const deleted = TTID.generate(updated, true) // mark deleted (final state)
//   TTID.decodeTime(deleted)              // { createdAt, updatedAt, deletedAt } (ms)
//   TTID.isTTID(id)                       // Date if valid, else null
//   TTID.isUUID('not-a-uuid')             // RegExpMatchArray if valid, else null
//
// The named functions are exported too, if you prefer them over the class.

const PRECISION = 10_000;
const BASE = 36;
const PLACEHOLDER = 'X';
const MIN_TIMESTAMP_MS = 1_577_836_800_000; // 2020-01-01T00:00:00.000Z
const MAX_TIMESTAMP_MS = 7_258_118_400_000; // 2200-01-01T00:00:00.000Z
const TTID_PATTERN = /^[A-Z0-9]{11}(-[A-Z0-9]{1,11}){0,2}$/i;
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Current high-resolution timestamp, scaled to preserve sub-ms precision. */
function timeNow() {
    return (performance.now() + performance.timeOrigin) * PRECISION;
}

/**
 * Decode the timestamps embedded in a TTID.
 * @param {string} _id
 * @returns {{ createdAt: number, updatedAt?: number, deletedAt?: number }}
 * @throws {Error} If the format is invalid or a segment is out of range.
 */
export function decodeTime(_id) {
    if (!TTID_PATTERN.test(_id)) throw new Error('Invalid Format!');

    const [created, updated, deleted] = _id.split('-');

    const convertToMilliseconds = (timeCode) => {
        const ms = Number((parseInt(timeCode, BASE) / PRECISION).toFixed(0));
        if (!isFinite(ms) || ms < MIN_TIMESTAMP_MS || ms > MAX_TIMESTAMP_MS) {
            throw new Error('Invalid timestamp encoding');
        }
        return ms;
    };

    const timestamps = { createdAt: convertToMilliseconds(created) };
    if (updated && updated !== PLACEHOLDER) timestamps.updatedAt = convertToMilliseconds(updated);
    if (deleted) timestamps.deletedAt = convertToMilliseconds(deleted);
    return timestamps;
}

/**
 * Validate a TTID.
 * @param {string} _id
 * @returns {Date | null} The creation `Date` if valid, else `null`.
 */
export function isTTID(_id) {
    if (!_id || _id.length > 36) return null;
    if (!TTID_PATTERN.test(_id)) return null;
    try {
        const { createdAt } = decodeTime(_id);
        return new Date(createdAt);
    } catch {
        return null;
    }
}

/**
 * Validate a UUID (any version or variant).
 * @param {string} _id
 * @returns {RegExpMatchArray | null}
 */
export function isUUID(_id) {
    return _id.match(UUID_PATTERN);
}

/**
 * Generate a new TTID, or advance an existing one through its lifecycle.
 * @param {string} [_id] An existing TTID to update or delete. Omit for a new one.
 * @param {boolean} [del=false] When `true`, marks the TTID as deleted.
 * @returns {string} The new or advanced TTID.
 * @throws {Error} If `_id` is already deleted (three segments) or not a valid TTID.
 */
export function generate(_id, del = false) {
    if (_id && isTTID(_id) && _id.split('-').length === 3) {
        throw new Error('This identifier can no longer be modified');
    }

    const time = timeNow();

    if (_id && isTTID(_id) && del) {
        const [created, updated] = _id.split('-');
        const deleted = time.toString(BASE);
        return `${created}-${updated ?? PLACEHOLDER}-${deleted}`.toUpperCase();
    }

    if (_id && isTTID(_id)) {
        const [created] = _id.split('-');
        const updated = time.toString(BASE);
        return `${created}-${updated}`.toUpperCase();
    }

    if (_id && !isTTID(_id)) throw new Error('Invalid TTID!');

    return time.toString(BASE).toUpperCase();
}

/** Static-method facade, mirroring the TTID library's default export. */
export default class TTID {
    static generate = generate;
    static decodeTime = decodeTime;
    static isTTID = isTTID;
    static isUUID = isUUID;
}