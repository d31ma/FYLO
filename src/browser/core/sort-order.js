import { BrowserPrefixIndexCodec, sha256Hex } from './prefix-index.js'

/**
 * The `$sort` of a query: one field and a direction, served from the field's
 * sortable numeric keys, with a keyset cursor (ADR 0023 sections 5 and 6).
 * `crates/fylo-query/src/sort_order.rs` is the Rust engine's version.
 */
export class BrowserSortOrder {
    static CURSOR_FORMAT = 'fys1'
    static CURSOR_PARTS = 5
    static DIGEST_HEX_DIGITS = 16
    static SORTABLE_VALUE = /^[0-9a-f]{16}$/

    /**
     * @param {string} field
     * @param {boolean} descending
     * @param {string} digest
     */
    constructor(field, descending, digest) {
        this.field = field
        this.descending = descending
        this.digest = digest
    }

    /**
     * The `$sort` of `query`, or null when it has none.
     * @param {Record<string, any>} query
     * @param {(field: string) => string} normalizeFieldPath
     * @returns {Promise<BrowserSortOrder | null>}
     */
    static async parse(query, normalizeFieldPath) {
        if (query.$sort === undefined) return null
        const [field, direction] = BrowserSortOrder.singleEntry(query.$sort)
        const descending = BrowserSortOrder.isDescending(direction)
        const digest = await BrowserSortOrder.digestOf(query)
        return new BrowserSortOrder(normalizeFieldPath(field), descending, digest)
    }

    /**
     * @param {unknown} sort
     * @returns {[string, unknown]}
     */
    static singleEntry(sort) {
        const entries = sort && typeof sort === 'object' ? Object.entries(sort) : []
        if (entries.length !== 1) {
            throw BrowserSortOrder.error(
                'EQUERY_INVALID',
                '$sort must be an object with exactly one field'
            )
        }
        return entries[0]
    }

    /**
     * @param {unknown} direction
     * @returns {boolean}
     */
    static isDescending(direction) {
        if (direction === 'asc') return false
        if (direction === 'desc') return true
        throw BrowserSortOrder.error('EQUERY_INVALID', '$sort direction must be "asc" or "desc"')
    }

    /**
     * @param {Record<string, any>} query
     * @returns {Promise<string>}
     */
    static async digestOf(query) {
        const scope = {
            $ops: query.$ops ?? null,
            $created: query.$created ?? null,
            $updated: query.$updated ?? null,
            $sort: query.$sort ?? null
        }
        const digest = await sha256Hex(JSON.stringify(scope))
        return digest.slice(0, BrowserSortOrder.DIGEST_HEX_DIGITS)
    }

    /** The index keys that hold this order. @returns {string} */
    keyPrefix() {
        return BrowserPrefixIndexCodec.prefix(this.field, this.descending ? 'nr' : 'n')
    }

    /**
     * @param {string} key
     * @returns {{ value: string, id: string } | null}
     */
    positionOfKey(key) {
        const rest = key.slice(this.keyPrefix().length)
        const slash = rest.indexOf('/')
        if (slash < 0) return null
        return { value: rest.slice(0, slash), id: decodeURIComponent(rest.slice(slash + 1)) }
    }

    /**
     * @param {{ value: string, id: string }} position
     * @returns {string}
     */
    keyOf(position) {
        return `${this.keyPrefix()}${position.value}/${encodeURIComponent(position.id)}`
    }

    /**
     * The smallest sortable key value of the document's sort field: its
     * smallest number ascending, its largest descending.
     * @param {Record<string, any>} document
     * @returns {string | null}
     */
    placingValue(document) {
        const value = BrowserSortOrder.valueAt(document, this.field)
        const items = Array.isArray(value) ? value : [value]
        const sortable = items
            .filter((item) => item !== undefined && (item === null || typeof item !== 'object'))
            .map((item) => BrowserPrefixIndexCodec.sortableValue(item, this.descending))
            .filter((item) => item !== null)
            .sort()
        return sortable[0] ?? null
    }

    /**
     * @param {Record<string, any>} document
     * @param {string} path
     * @returns {unknown}
     */
    static valueAt(document, path) {
        /** @type {any} */
        let current = document
        for (const segment of path.split('/')) {
            if (!current || typeof current !== 'object' || Array.isArray(current)) return undefined
            current = current[segment]
        }
        return current
    }

    /**
     * @param {{ value: string, id: string }} position
     * @returns {string}
     */
    cursor(position) {
        return [
            BrowserSortOrder.CURSOR_FORMAT,
            this.descending ? 'd' : 'a',
            position.value,
            encodeURIComponent(position.id),
            this.digest
        ].join('.')
    }

    /**
     * The position a `$after` cursor names, checked against this order.
     * @param {unknown} cursor
     * @returns {{ value: string, id: string }}
     */
    parseCursor(cursor) {
        const parts = typeof cursor === 'string' ? cursor.split('.') : []
        if (parts.length !== BrowserSortOrder.CURSOR_PARTS || parts[0] !== 'fys1') {
            throw BrowserSortOrder.error('EINVALIDCURSOR', 'the cursor does not parse')
        }
        if (parts[1] !== (this.descending ? 'd' : 'a') || parts[4] !== this.digest) {
            throw BrowserSortOrder.error(
                'EINVALIDCURSOR',
                'the cursor belongs to a different query; restart from the first page'
            )
        }
        if (!BrowserSortOrder.SORTABLE_VALUE.test(parts[2])) {
            throw BrowserSortOrder.error('EINVALIDCURSOR', "the cursor's sort value does not parse")
        }
        return { value: parts[2], id: decodeURIComponent(parts[3]) }
    }

    /**
     * @param {string} code
     * @param {string} message
     * @returns {Error}
     */
    static error(code, message) {
        return Object.assign(new Error(message), { code })
    }
}
