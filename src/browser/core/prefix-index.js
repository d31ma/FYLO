import { stringifyStoredValue } from '../../storage/value-codec.js'
import TTID from '../vendor/ttid.mjs'
import { join } from './path.js'

/**
 * Browser-compatible port of `src/storage/prefix-index.js`. Stores the same
 * `manifest.json` + `keys.snapshot` + `keys.wal` shape so a future migration
 * tool can move index files between the server and a browser-OPFS deployment
 * unchanged. The crypto hash uses Web Crypto so it works on Bun, Node 18+ and
 * every modern browser. Browser field encryption is intentionally not wired
 * into this module yet; encrypted-field indexes will need a browser-safe
 * crypto adapter, not the server cipher singleton.
 *
 * @typedef {import('../../types/vendor.js').TTID} TTIDValue
 * @typedef {import('../../query/types.js').Operand} Operand
 * @typedef {import('./filesystem.js').FyloFilesystem} FyloFilesystem
 */

const LOCAL_FS_FORMAT = 'fylo.local-fs.index.v1'
const LOCAL_FS_MANIFEST = 'manifest.json'
const LOCAL_FS_SNAPSHOT = 'keys.snapshot'
const LOCAL_FS_WAL = 'keys.wal'
const LOCAL_FS_WAL_COMPACT_BYTES = 1_048_576

const MAX_STRING_PREFIX_BYTES = 180
const MAX_NGRAM_SOURCE_CHARS = 512
const NGRAM_SIZE = 3
const UINT64_MAX = (1n << 64n) - 1n
const SIGN_MASK = 1n << 63n

const ENCODER = new TextEncoder()
const DECODER = new TextDecoder()

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
 * @param {Uint8Array} bytes
 * @returns {string}
 */
function bytesToHex(bytes) {
    let hex = ''
    for (const byte of bytes) hex += byte.toString(16).padStart(2, '0')
    return hex
}

/**
 * Web Crypto-backed SHA-256. Available on Bun, Node ≥ 18 and every modern
 * browser. Identical bytes-out as `Bun.CryptoHasher('sha256')` so server and
 * browser produce the same index keys for the same input value.
 *
 * @param {string} value
 * @returns {Promise<string>}
 */
export async function sha256Hex(value) {
    const buffer = await crypto.subtle.digest('SHA-256', ENCODER.encode(value))
    return bytesToHex(new Uint8Array(buffer))
}

/**
 * @param {string} value
 * @returns {Promise<string>}
 */
async function lookupToken(value) {
    const encoded = encodeSegment(value)
    if (ENCODER.encode(encoded).byteLength <= MAX_STRING_PREFIX_BYTES) return encoded
    return `h_${await sha256Hex(value)}`
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
 * @param {string} value
 * @returns {string[]}
 */
function trigrams(value) {
    const source = value.slice(0, MAX_NGRAM_SOURCE_CHARS)
    const chars = Array.from(source)
    if (chars.length < NGRAM_SIZE) return []
    const grams = new Set()
    for (let i = 0; i <= chars.length - NGRAM_SIZE; i++) {
        grams.add(chars.slice(i, i + NGRAM_SIZE).join(''))
    }
    return [...grams]
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
 * @returns {TTIDValue}
 */
function docIdFromKey(prefix, key) {
    const suffix = key.slice(prefix.length)
    const segments = suffix.split('/')
    const docId = decodeSegment(segments.at(-1) ?? '')
    if (!TTID.isTTID(docId)) throw new Error(`Invalid document ID: ${docId}`)
    return docId
}

/**
 * @param {string} collection
 * @param {string} fieldPath
 * @param {unknown} value
 * @returns {Promise<string>}
 */
async function queryLookupValue(collection, fieldPath, value) {
    void collection
    void fieldPath
    const stored = stringifyStoredValue(value)
    return lookupToken(stored)
}

/**
 * Encodes document field values into prefix-searchable index keys and query
 * lookup prefixes. Algorithmically identical to the server `PrefixIndexCodec`,
 * but uses the Web Crypto `sha256Hex` so it runs everywhere.
 */
export class BrowserPrefixIndexCodec {
    /**
     * @param {string} fieldPath
     * @param {string} kind
     * @param {string} value
     * @param {TTIDValue} docId
     * @returns {string}
     */
    static key(fieldPath, kind, value, docId) {
        if (!TTID.isTTID(docId)) throw new Error(`Invalid document ID: ${docId}`)
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
     * @param {TTIDValue} docId
     * @param {Record<string, any>} data
     * @returns {Promise<string[]>}
     */
    static async entriesForDocument(collection, docId, data) {
        /** @type {string[]} */
        const entries = []
        /** @type {string[]} */
        const exactEntries = []
        /**
         * @param {string} fieldPath
         * @param {unknown} raw
         */
        const addValue = async (fieldPath, raw) => {
            const value = stringifyStoredValue(raw)
            /**
             * @param {string} kind
             * @param {string} plannedValue
             * @returns {string}
             */
            const planKey = (kind, plannedValue) => this.key(fieldPath, kind, plannedValue, docId)

            exactEntries.push(this.key(fieldPath, 'eq', await lookupToken(value), docId))

            const numeric = numericValue(raw)
            const sortable = numeric === null ? '' : sortableFloat64(numeric)
            if (sortable) {
                entries.push(planKey('n', sortable))
                entries.push(planKey('nr', reverseSortable(sortable)))
            }

            if (typeof raw !== 'string') return
            const encoded = encodeSegment(value)
            if (ENCODER.encode(encoded).byteLength > MAX_STRING_PREFIX_BYTES) return
            entries.push(planKey('f', encoded))
            entries.push(planKey('r', encodeSegment(reverseString(value))))
            for (const gram of trigrams(value)) {
                entries.push(planKey('g3', encodeSegment(gram)))
            }
        }

        /**
         * @param {Record<string, any>} target
         * @param {string=} parentField
         */
        const walk = async (target, parentField) => {
            for (const field of Object.keys(target)) {
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
        entries.push(...exactEntries)
        return [...new Set(entries)]
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
            // `_` is a one-character wildcard. Existing prefix/reverse/ngram
            // indexes cannot encode that positional constraint safely, so use
            // the bounded in-memory matcher rather than risk false negatives.
            if (pattern.includes('_')) return null
            const wildcards = (pattern.match(/%/g) ?? []).length
            if (wildcards === 0) {
                return [
                    {
                        kind: 'eq',
                        valuePrefix: `${await queryLookupValue(collection, fieldPath, pattern)}/`
                    }
                ]
            }
            if (wildcards === 1 && pattern.endsWith('%')) {
                return [
                    {
                        kind: 'f',
                        valuePrefix: encodeSegment(pattern.slice(0, -1))
                    }
                ]
            }
            if (wildcards === 1 && pattern.startsWith('%')) {
                return [
                    {
                        kind: 'r',
                        valuePrefix: encodeSegment(reverseString(pattern.slice(1)))
                    }
                ]
            }
            if (
                wildcards === 2 &&
                pattern.startsWith('%') &&
                pattern.endsWith('%') &&
                pattern.length > 2
            ) {
                const needle = pattern.slice(1, -1)
                if (Array.from(needle).length >= NGRAM_SIZE) {
                    const planned = trigrams(needle)[0]
                    return [
                        {
                            kind: 'g3',
                            valuePrefix: `${encodeSegment(planned ?? needle.slice(0, 3))}/`
                        }
                    ]
                }
            }
            return null
        }
        /** @type {Array<{ kind: string, valuePrefix: string, range: { op: '$gt' | '$gte' | '$lt' | '$lte', value: string } }>} */
        const rangeEntries = []
        for (const operator of /** @type {const} */ (['$gt', '$gte', '$lt', '$lte'])) {
            const raw = operand[operator]
            if (raw === undefined) continue
            const numeric = numericValue(raw)
            const sortable = numeric === null ? '' : sortableFloat64(numeric)
            if (!sortable) return null
            if (operator === '$gt' || operator === '$gte') {
                rangeEntries.push({
                    kind: 'n',
                    valuePrefix: '',
                    range: { op: operator, value: sortable }
                })
            } else {
                rangeEntries.push({
                    kind: 'nr',
                    valuePrefix: '',
                    range: { op: operator, value: reverseSortable(sortable) }
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
function completeLines(text) {
    if (!text) return []
    const complete = text.endsWith('\n') ? text : text.slice(0, text.lastIndexOf('\n') + 1)
    return complete.split('\n').map(stripTrailingCarriageReturn).filter(Boolean)
}

/**
 * @param {string} line
 * @returns {string}
 */
function stripTrailingCarriageReturn(line) {
    return line.endsWith('\r') ? line.slice(0, -1) : line
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

/**
 * @param {string} line
 * @returns {{ op: '+' | '-', key: string } | null}
 */
function parseWalMutation(line) {
    const operation = line[0]
    if ((operation !== '+' && operation !== '-') || line[1] !== '\t') return null
    const key = line.slice(2)
    return key ? { op: operation, key } : null
}

/**
 * @param {FyloFilesystem} fs
 * @param {string} path
 * @returns {Promise<string>}
 */
async function readTextIfExists(fs, path) {
    if (!(await fs.exists(path))) return ''
    return await fs.readText(path)
}

/**
 * @param {FyloFilesystem} fs
 * @param {string} path
 * @param {string} data
 * @returns {Promise<void>}
 */
async function writeIfMissing(fs, path, data) {
    if (await fs.exists(path)) return
    await fs.writeText(path, data)
}

/**
 * Browser/browser port of FYLO's local-filesystem prefix index. Stores the same
 * compact zero-payload key format on disk so a future migration tool can move
 * collections between server and browser deployments unchanged.
 *
 * Composes over any `FyloFilesystem`. Used by `BrowserCore` to satisfy the
 * prefix-index shape that `BrowserQueryEngine` calls into.
 */
export class BrowserPrefixIndex {
    /**
     * @param {FyloFilesystem} fs
     * @param {(collection: string) => string} rootForCollection
     */
    constructor(fs, rootForCollection) {
        this.fs = fs
        this.rootForCollection = rootForCollection
        /** @type {Map<string, Uint8Array>} */
        this.snapshotCache = new Map()
    }

    /**
     * @param {string} collection
     * @returns {string}
     */
    root(collection) {
        return join(this.rootForCollection(collection), 'index')
    }

    /** @param {string} collection @returns {string} */
    manifestPath(collection) {
        return join(this.root(collection), LOCAL_FS_MANIFEST)
    }

    /** @param {string} collection @returns {string} */
    snapshotPath(collection) {
        return join(this.root(collection), LOCAL_FS_SNAPSHOT)
    }

    /** @param {string} collection @returns {string} */
    walPath(collection) {
        return join(this.root(collection), LOCAL_FS_WAL)
    }

    /** @param {string} collection @returns {Promise<void>} */
    async ensureCollection(collection) {
        await this.fs.mkdir(this.root(collection), { recursive: true })
        await writeIfMissing(
            this.fs,
            this.manifestPath(collection),
            `${JSON.stringify({ format: LOCAL_FS_FORMAT, createdAt: Date.now() })}\n`
        )
        await writeIfMissing(this.fs, this.snapshotPath(collection), '')
        await writeIfMissing(this.fs, this.walPath(collection), '')
    }

    /** @param {string} collection @returns {Promise<void>} */
    async resetCollection(collection) {
        this.snapshotCache.delete(collection)
        await this.fs.rmdir(this.root(collection), { recursive: true })
        await this.ensureCollection(collection)
    }

    /**
     * Reads (and caches) the snapshot bytes for `collection`.
     *
     * The cache is invalidated on every mutation by `appendMutations` /
     * `compact`. Hot queries pay one read per snapshot regeneration.
     *
     * @param {string} collection
     * @returns {Promise<Uint8Array>}
     */
    async readSnapshotBytes(collection) {
        const cached = this.snapshotCache.get(collection)
        if (cached) return cached
        const path = this.snapshotPath(collection)
        const bytes = (await this.fs.exists(path))
            ? await this.fs.readBytes(path)
            : new Uint8Array(0)
        this.snapshotCache.set(collection, bytes)
        return bytes
    }

    /**
     * Binary search returning the byte offset of the first key ≥ `prefix`.
     * Matches `src/storage/prefix-index.js` exactly.
     *
     * @param {Uint8Array} bytes
     * @param {string} prefix
     * @returns {number}
     */
    findFirstKeyAtOrAfter(bytes, prefix) {
        if (bytes.length === 0) return 0
        let lo = 0
        let hi = bytes.length
        while (lo < hi) {
            const mid = Math.floor((lo + hi) / 2)
            let start = mid
            while (start > 0 && bytes[start - 1] !== 0x0a) start--
            let end = mid
            while (end < bytes.length && bytes[end] !== 0x0a) end++
            const key = DECODER.decode(bytes.subarray(start, end))
            if (key < prefix) lo = end + 1
            else hi = start
        }
        while (lo > 0 && lo < bytes.length && bytes[lo - 1] !== 0x0a) lo--
        return lo
    }

    /**
     * Loads the full key set (snapshot + WAL) into memory. Used during
     * compaction and for `countDocuments`. Not the hot query path.
     *
     * @param {string} collection
     * @returns {Promise<Set<string>>}
     */
    async loadKeySet(collection) {
        await this.ensureCollection(collection)
        const bytes = await this.readSnapshotBytes(collection)
        const keys = new Set(completeLines(DECODER.decode(bytes)))
        for (const line of completeLines(
            await readTextIfExists(this.fs, this.walPath(collection))
        )) {
            const mutation = parseWalMutation(line)
            if (!mutation) continue
            if (mutation.op === '+') keys.add(mutation.key)
            else keys.delete(mutation.key)
        }
        return keys
    }

    /**
     * @param {string} collection
     * @param {{ op: '+' | '-', key: string }[]} mutations
     * @returns {Promise<void>}
     */
    async appendMutations(collection, mutations) {
        if (mutations.length === 0) return
        await this.ensureCollection(collection)
        await this.fs.appendText(this.walPath(collection), serializeWal(mutations))
        await this.compactIfNeeded(collection)
    }

    /** @param {string} collection @returns {Promise<void>} */
    async compactIfNeeded(collection) {
        if (!(await this.fs.exists(this.walPath(collection)))) return
        const wal = await this.fs.readText(this.walPath(collection))
        if (ENCODER.encode(wal).byteLength < LOCAL_FS_WAL_COMPACT_BYTES) return
        await this.compact(collection)
    }

    /**
     * Merges the WAL into the sorted snapshot. The in-memory snapshot cache is
     * dropped so the next query re-reads the freshly compacted file.
     *
     * @param {string} collection
     * @returns {Promise<void>}
     */
    async compact(collection) {
        const keys = [...(await this.loadKeySet(collection))].sort()
        await this.fs.writeText(this.snapshotPath(collection), serializeSnapshot(keys))
        await this.fs.writeText(this.walPath(collection), '')
        this.snapshotCache.delete(collection)
    }

    /**
     * @param {string} collection
     * @param {TTIDValue} docId
     * @param {Record<string, any>} doc
     * @returns {Promise<void>}
     */
    async putDocument(collection, docId, doc) {
        const keys = await BrowserPrefixIndexCodec.entriesForDocument(collection, docId, doc)
        await this.appendMutations(
            collection,
            keys.map((key) => ({ op: '+', key }))
        )
        this.snapshotCache.delete(collection)
    }

    /**
     * @param {string} collection
     * @param {TTIDValue} docId
     * @param {Record<string, any>} doc
     * @returns {Promise<void>}
     */
    async removeDocument(collection, docId, doc) {
        const keys = await BrowserPrefixIndexCodec.entriesForDocument(collection, docId, doc)
        await this.appendMutations(
            collection,
            keys.map((key) => ({ op: '-', key }))
        )
        this.snapshotCache.delete(collection)
    }

    /**
     * Counts distinct document IDs visible in the merged snapshot + WAL.
     *
     * @param {string} collection
     * @returns {Promise<number>}
     */
    async countDocuments(collection) {
        const docIds = new Set()
        for (const key of await this.loadKeySet(collection)) {
            const segments = key.split('/')
            if (segments.length < 2) continue
            docIds.add(segments.at(-1))
        }
        return docIds.size
    }

    /**
     * @param {string} collection
     * @param {string} fieldPath
     * @param {Operand} operand
     * @returns {Promise<Set<TTIDValue> | null>}
     */
    async candidateDocIds(collection, fieldPath, operand) {
        const prefixes = await BrowserPrefixIndexCodec.queryPrefixes(collection, fieldPath, operand)
        if (!prefixes) return null
        await this.ensureCollection(collection)
        const overlay = await this.loadWalOverlay(collection)
        const bytes = await this.readSnapshotBytes(collection)
        /** @type {Set<TTIDValue> | null} */
        let candidates = null
        for (const entry of prefixes) {
            const rootPrefix = BrowserPrefixIndexCodec.prefix(fieldPath, entry.kind)
            const fullPrefix = BrowserPrefixIndexCodec.prefix(
                fieldPath,
                entry.kind,
                entry.valuePrefix
            )
            const offset = this.findFirstKeyAtOrAfter(bytes, fullPrefix)
            /** @type {Set<TTIDValue>} */
            const next = new Set()
            for (const key of streamKeysFrom(bytes, offset)) {
                if (!key.startsWith(fullPrefix)) break
                if (overlay.removed.has(key)) continue
                if (!includeKeyInRange(key, entry.range)) continue
                next.add(docIdFromKey(rootPrefix, key))
            }
            for (const key of overlay.added) {
                if (!key.startsWith(fullPrefix)) continue
                if (!includeKeyInRange(key, entry.range)) continue
                next.add(docIdFromKey(rootPrefix, key))
            }
            candidates = intersect(candidates, next)
        }
        return candidates
    }

    /**
     * @param {string} collection
     * @returns {Promise<{ added: Set<string>, removed: Set<string> }>}
     */
    async loadWalOverlay(collection) {
        const added = new Set()
        const removed = new Set()
        for (const line of completeLines(
            await readTextIfExists(this.fs, this.walPath(collection))
        )) {
            const mutation = parseWalMutation(line)
            if (!mutation) continue
            if (mutation.op === '+') {
                added.add(mutation.key)
                removed.delete(mutation.key)
            } else {
                added.delete(mutation.key)
                removed.add(mutation.key)
            }
        }
        return { added, removed }
    }
}

/**
 * Streams sorted keys from `bytes` starting at `offset`. Sync iterator —
 * callers compare each yielded key against the prefix they care about and
 * `break` when the prefix is exceeded.
 *
 * @param {Uint8Array} bytes
 * @param {number} offset
 * @returns {Generator<string, void, unknown>}
 */
function* streamKeysFrom(bytes, offset) {
    let cursor = offset
    while (cursor < bytes.length) {
        let end = cursor
        while (end < bytes.length && bytes[end] !== 0x0a) end++
        const key = DECODER.decode(bytes.subarray(cursor, end))
        if (key.length > 0) yield key
        cursor = end + 1
    }
}

/**
 * Set intersection compatible with the server's pattern: a `null` left side
 * means "no constraint yet", so the first constraint wins. Subsequent
 * constraints intersect against the running set.
 *
 * @template T
 * @param {Set<T> | null} current
 * @param {Set<T>} next
 * @returns {Set<T>}
 */
function intersect(current, next) {
    if (current === null) return next
    const out = new Set()
    for (const value of next) if (current.has(value)) out.add(value)
    return out
}

/**
 * @param {string} key
 * @param {{ op: '$gt' | '$gte' | '$lt' | '$lte', value: string } | undefined} range
 * @returns {boolean}
 */
function includeKeyInRange(key, range) {
    if (!range) return true
    const value = BrowserPrefixIndexCodec.rangeValueFromKey(key)
    if (range.op === '$gt') return value > range.value
    if (range.op === '$gte') return value >= range.value
    if (range.op === '$lt') return value > range.value
    if (range.op === '$lte') return value >= range.value
    return true
}
