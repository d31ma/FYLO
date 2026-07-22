import { Parser } from './parser.js'

/** @template T @param {T} value @param {WeakSet<object>} [seen] @returns {T} */
function deepFreeze(value, seen = new WeakSet()) {
    if (!value || typeof value !== 'object' || seen.has(value)) return value
    seen.add(value)
    for (const child of Object.values(value)) deepFreeze(child, seen)
    return Object.freeze(value)
}

/** @param {Record<string, any> | undefined} query */
function accessPaths(query) {
    if (!query?.$ops?.length) return [{ kind: 'document-scan' }]
    const access = []
    for (const operation of query.$ops) {
        for (const [field, operand] of Object.entries(operation)) {
            if (!operand || typeof operand !== 'object') continue
            const operators = Object.keys(operand)
            const indexable = operators.some((operator) =>
                ['$eq', '$gt', '$gte', '$lt', '$lte', '$like', '$contains'].includes(operator)
            )
            access.push(
                indexable
                    ? { kind: 'prefix-index', field, operators }
                    : { kind: 'document-filter', field, operators }
            )
        }
    }
    return access.length > 0 ? access : [{ kind: 'document-scan' }]
}

/**
 * Immutable parsed SQL plan. Execution receives a clone of `ast`, allowing a
 * prepared statement to be reused even when legacy execution paths remove
 * collection routing fields from their working copy.
 */
export class FyloQueryPlanner {
    /** @param {string} input */
    prepare(input) {
        if (typeof input !== 'string' || input.trim().length === 0) {
            throw new Error('SQL statement must be a non-empty string')
        }
        let sql = input.trim()
        let explain = false
        let analyze = false
        const explainMatch = sql.match(/^EXPLAIN(?:\s+(ANALYZE))?\s+/i)
        if (explainMatch) {
            explain = true
            analyze = Boolean(explainMatch[1])
            sql = sql.slice(explainMatch[0].length).trim()
        }
        const operation = sql.match(/^(SELECT|INSERT|UPDATE|DELETE|CREATE|DROP)\b/i)?.[1]
        if (!operation) throw new Error('Missing SQL Operation')
        const ast = deepFreeze(/** @type {any} */ (Parser.parse(sql)))
        const collection = String(ast.$collection ?? ast.$leftCollection ?? '')
        const query = operation.toUpperCase() === 'UPDATE' ? ast.$where : ast
        return deepFreeze({
            sql,
            operation: operation.toUpperCase(),
            collection,
            ast,
            explain,
            analyze,
            access: accessPaths(query)
        })
    }

    /** @param {ReturnType<FyloQueryPlanner['prepare']>} plan */
    describe(plan) {
        return {
            operation: plan.operation,
            collection: plan.collection,
            access: plan.access.map((step) => ({ ...step })),
            executed: false
        }
    }
}

export class FyloPreparedStatement {
    /**
     * @param {FyloQueryPlanner} planner
     * @param {ReturnType<FyloQueryPlanner['prepare']>} plan
     * @param {(plan: ReturnType<FyloQueryPlanner['prepare']>) => Promise<unknown>} execute
     */
    constructor(planner, plan, execute) {
        this.planner = planner
        this.plan = plan
        this.executePlan = execute
    }

    explain() {
        return this.planner.describe(this.plan)
    }

    async execute() {
        return await this.executePlan(this.plan)
    }
}
