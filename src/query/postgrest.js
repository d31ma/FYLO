import { safeRecord } from './safe-record.js'

/**
 * PostgREST-style filter grammar → FYLO query translation.
 *
 * Parses the `field=op.value` search syntax (`role=eq.admin&age=gte.30`) into
 * a `findDocs`-shaped query. Operators: `eq ne gt gte lt lte like contains`,
 * plus `limit`, `select`, and `onlyIds`. Kept as a standalone module so any
 * surface that wants the familiar syntax (e.g. the Explorer's filter bar) can
 * share one grammar.
 */

/**
 * @param {string} search Search string with or without a leading `?`
 * @returns {Record<string, any>}
 */
export function queryFromSearch(search) {
    /** @type {Record<string, any>} */
    const query = {}
    // PostgREST semantics: every filter must hold (AND). FYLO ORs the entries
    // of $ops and ANDs the keys within one entry, so all filters merge into a
    // single entry; repeating a field merges its operators (age=gte.18&age=lt.30).
    /** @type {Record<string, any>} */
    const filters = safeRecord()
    let hasFilters = false
    for (const [key, value] of new URLSearchParams(search)) {
        if (key === 'limit') {
            const limit = Number(value)
            if (!Number.isInteger(limit) || limit < 0) throw new Error('Invalid limit')
            query.$limit = limit
            continue
        }
        if (key === 'select') {
            query.$select = value
                .split(',')
                .map((item) => item.trim())
                .filter(Boolean)
            continue
        }
        if (key === 'onlyIds') {
            query.$onlyIds = value === 'true'
            continue
        }
        const operand = parseFilterOperand(value)
        if (operand) {
            const current = Object.hasOwn(filters, key) ? filters[key] : safeRecord()
            for (const [operator, expected] of Object.entries(operand)) {
                current[operator] = expected
            }
            filters[key] = current
            hasFilters = true
        }
    }
    if (hasFilters) query.$ops = [filters]
    return query
}

/**
 * @param {string} value
 * @returns {Record<string, any> | null}
 */
function parseFilterOperand(value) {
    const dot = value.indexOf('.')
    if (dot === -1) return { $eq: coerceValue(value) }
    const op = value.slice(0, dot)
    const raw = value.slice(dot + 1)
    const map = {
        eq: '$eq',
        ne: '$ne',
        gt: '$gt',
        gte: '$gte',
        lt: '$lt',
        lte: '$lte',
        like: '$like',
        contains: '$contains'
    }
    const mapped = /** @type {Record<string, string>} */ (map)[op]
    if (!mapped) return { $eq: coerceValue(value) }
    return { [mapped]: mapped === '$like' ? raw : coerceValue(raw) }
}

/**
 * @param {string} value
 * @returns {string | number | boolean | null}
 */
function coerceValue(value) {
    if (value === 'true') return true
    if (value === 'false') return false
    if (value === 'null') return null
    if (/^-?\d+(\.\d+)?$/.test(value)) return Number(value)
    return value
}
