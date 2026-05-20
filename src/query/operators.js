import { Cipher } from '../security/cipher.js'

/**
 * @typedef {import('./types.js').StoreQuery<Record<string, any>>} StoreQuery
 */

/** @type {(keyof import('./types.js').Operand)[]} */
const ENCRYPTED_FIELD_OPS = ['$ne', '$gt', '$gte', '$lt', '$lte', '$like', '$contains']

/**
 * Utility helpers for explaining which prefix-index expressions a FYLO query
 * can use. This is primarily diagnostic/admin-facing; execution happens in
 * the storage query engine.
 */
export class Query {
    /**
     * Builds diagnostic prefix-index expressions that can satisfy a structured FYLO query.
     * @param {string} collection
     * @param {StoreQuery} query
     * @returns {Promise<string[]>}
     */
    static async getExprs(collection, query) {
        /** @type {Set<string>} */
        let expressions = new Set()
        if (query.$ops) {
            for (const operation of query.$ops) {
                for (const column in operation) {
                    /** @type {import('./types.js').Operand | undefined} */
                    const operand = operation[column]
                    if (!operand) continue
                    const fieldPath = String(column).replaceAll('.', '/')
                    const encrypted =
                        Cipher.isConfigured() && Cipher.isEncryptedField(collection, fieldPath)
                    if (encrypted) {
                        for (const operator of ENCRYPTED_FIELD_OPS) {
                            if (operand[operator] !== undefined) {
                                throw new Error(
                                    `Operator ${operator} is not supported on encrypted field "${String(column)}"`
                                )
                            }
                        }
                    }
                    if (operand.$eq) {
                        const lookupValue = encrypted
                            ? await Cipher.blindIndex(String(operand.$eq).replaceAll('/', '%2F'))
                            : operand.$eq
                        expressions.add(`${fieldPath}/eq/${lookupValue}/**/*`)
                    }
                    if (operand.$ne) expressions.add(`${fieldPath}/**/*`)
                    if (operand.$gt) expressions.add(`${fieldPath}/n/**/*`)
                    if (operand.$gte) expressions.add(`${fieldPath}/n/**/*`)
                    if (operand.$lt) expressions.add(`${fieldPath}/nr/**/*`)
                    if (operand.$lte) expressions.add(`${fieldPath}/nr/**/*`)
                    if (operand.$like)
                        expressions.add(`${fieldPath}/f/${operand.$like.replaceAll('%', '*')}/**/*`)
                    if (operand.$contains !== undefined)
                        expressions.add(`${fieldPath}/eq/${String(operand.$contains)}/**/*`)
                }
            }
        } else expressions = new Set([`**/*`])
        return Array.from(expressions)
    }
}
