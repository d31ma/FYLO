/**
 * @fileoverview Expression evaluation for RLS rules.
 *
 * Two surface APIs:
 * - `evaluatePredicate(expr, context)` for explicit operator trees
 *   ($eq/$ne/$gt/$lt/$gte/$lte/$in/$and/$or/$not, plus boolean literals).
 *   Used for `apply_when` and `insert.predicate`.
 * - `evaluateFilter(filter, root, context)` for `{field: value}` shorthand
 *   maps. Used for `read.filter` / `update.filter` / `delete.filter` —
 *   each value may be a `%%scope.path` reference; substituted then
 *   compared field-by-field against `root` (the doc).
 * - `resolveFilter(filter, context)` substitutes `%%vars` without matching;
 *   returned shape is suitable for query merge or for matching by callers.
 *
 * Variable scopes:
 * - `%%user.X`     — auth context (subjectId, tenantId, roles, …)
 * - `%%root.X`     — the doc under consideration (for filters/predicates)
 * - `%%request.X`  — request-time metadata (action, docId, etc.)
 */

const VAR_PATTERN = /^%%([a-zA-Z][a-zA-Z0-9_]*)\.([a-zA-Z_][a-zA-Z0-9_.]*)$/

/**
 * Resolve a single `%%scope.path` reference against the context. Returns
 * the original string if not a variable reference.
 * @param {string} ref
 * @param {Record<string, any>} context
 */
function resolveVariable(ref, context) {
    const match = VAR_PATTERN.exec(ref)
    if (!match) return ref
    const [, scope, path] = match
    let cur = context[scope]
    if (cur === null || cur === undefined) return undefined
    for (const seg of path.split('.')) {
        if (cur === null || cur === undefined) return undefined
        cur = cur[seg]
    }
    return cur
}

/**
 * Recursively substitute `%%vars` inside a value tree.
 * @param {unknown} value
 * @param {Record<string, any>} context
 * @returns {unknown}
 */
export function substitute(value, context) {
    if (typeof value === 'string') {
        const resolved = resolveVariable(value, context)
        // Preserve literal strings; only var-refs may resolve to undefined.
        return VAR_PATTERN.test(value) ? resolved : value
    }
    if (Array.isArray(value)) return value.map((v) => substitute(v, context))
    if (value && typeof value === 'object') {
        /** @type {Record<string, unknown>} */
        const out = {}
        for (const [k, v] of Object.entries(value)) out[k] = substitute(v, context)
        return out
    }
    return value
}

/**
 * Structural deep-equality for primitives, arrays, and plain objects.
 * @param {unknown} a
 * @param {unknown} b
 * @returns {boolean}
 */
function deepEqual(a, b) {
    if (a === b) return true
    if (a === null || b === null || a === undefined || b === undefined) return false
    if (typeof a !== typeof b) return false
    if (typeof a !== 'object') return false
    if (Array.isArray(a) !== Array.isArray(b)) return false
    if (Array.isArray(a) && Array.isArray(b)) {
        if (a.length !== b.length) return false
        return a.every((v, i) => deepEqual(v, b[i]))
    }
    const ao = /** @type {Record<string, unknown>} */ (a)
    const bo = /** @type {Record<string, unknown>} */ (b)
    const ak = Object.keys(ao)
    const bk = Object.keys(bo)
    if (ak.length !== bk.length) return false
    return ak.every((k) => deepEqual(ao[k], bo[k]))
}

/** @type {Record<string, (a: any, b: any) => boolean>} */
const COMPARATORS = {
    $eq: (a, b) => deepEqual(a, b),
    $ne: (a, b) => !deepEqual(a, b),
    $gt: (a, b) => Number(a) > Number(b),
    $lt: (a, b) => Number(a) < Number(b),
    $gte: (a, b) => Number(a) >= Number(b),
    $lte: (a, b) => Number(a) <= Number(b),
    $in: (item, list) => Array.isArray(list) && list.some((x) => deepEqual(x, item))
}

/**
 * Evaluate a predicate expression. Boolean literals pass through; objects
 * may contain logical ops ($and/$or/$not) or comparator ops ($eq/$gt/etc.)
 * with a 2-element operand array. Unknown shapes throw.
 *
 * @param {unknown} expr
 * @param {Record<string, any>} context
 * @returns {boolean}
 */
export function evaluatePredicate(expr, context) {
    if (typeof expr === 'boolean') return expr
    if (expr === null || expr === undefined) return true
    if (typeof expr !== 'object') return Boolean(expr)

    const expression = /** @type {Record<string, unknown>} */ (expr)

    if ('$and' in expression) {
        const list = /** @type {unknown[]} */ (expression.$and)
        if (!Array.isArray(list)) throw new Error(`'$and' expects an array`)
        return list.every((sub) => evaluatePredicate(sub, context))
    }
    if ('$or' in expression) {
        const list = /** @type {unknown[]} */ (expression.$or)
        if (!Array.isArray(list)) throw new Error(`'$or' expects an array`)
        return list.some((sub) => evaluatePredicate(sub, context))
    }
    if ('$not' in expression) return !evaluatePredicate(expression.$not, context)

    for (const operator of Object.keys(expression)) {
        const comparator = COMPARATORS[operator]
        if (!comparator) continue
        const operands = expression[operator]
        if (!Array.isArray(operands) || operands.length !== 2) {
            throw new Error(`Comparator '${operator}' expects exactly 2 operands`)
        }
        const [leftOperand, rightOperand] = operands.map((value) => substitute(value, context))
        return comparator(leftOperand, rightOperand)
    }

    throw new Error(`Unknown predicate expression: ${JSON.stringify(expr)}`)
}

/**
 * Evaluate a `{field: value}` filter against `root`. Each value may be a
 * `%%scope.path` reference; substituted then compared with deep-equality.
 * Empty filter passes.
 *
 * @param {Record<string, unknown> | null | undefined} filter
 * @param {Record<string, unknown>} root
 * @param {Record<string, any>} context
 * @returns {boolean}
 */
export function evaluateFilter(filter, root, context) {
    if (!filter || Object.keys(filter).length === 0) return true
    for (const [field, raw] of Object.entries(filter)) {
        const expected = substitute(raw, context)
        if (!deepEqual(root[field], expected)) return false
    }
    return true
}

/**
 * Resolve `%%vars` in a filter without matching. Returns a literal-only
 * filter object suitable for callers that need to compare or merge.
 *
 * @param {Record<string, unknown> | null | undefined} filter
 * @param {Record<string, any>} context
 * @returns {Record<string, unknown>}
 */
export function resolveFilter(filter, context) {
    if (!filter) return {}
    return /** @type {Record<string, unknown>} */ (substitute(filter, context))
}
