import { Cipher } from '../security/cipher.js'
import { matchesLike } from '../query/like.js'

/**
 * @typedef {import('../types/vendor.js').TTID} TTIDValue
 * @typedef {import('../query/types.js').Operand} Operand
 * @typedef {import('../query/types.js').QueryOperation<Record<string, any>>} QueryOperation
 * @typedef {import('../query/types.js').StoreQuery<Record<string, any>>} StoreQuery
 * @typedef {import('./types.js').PrefixIndexStore} PrefixIndexStore
 * @typedef {{ index: PrefixIndexStore }} FilesystemQueryContext
 */

/**
 * Query evaluator for filesystem-backed documents, combining prefix-index
 * candidate selection with in-memory predicate checks.
 */
export class FilesystemQueryEngine {
    /** @type {FilesystemQueryContext} */
    context
    /**
     * @param {FilesystemQueryContext} context
     */
    constructor(context) {
        this.context = context
    }
    /**
     * Reads nested document values using FYLO slash or dot paths.
     * @param {Record<string, any>} target
     * @param {string} fieldPath
     * @returns {unknown}
     */
    getValueByPath(target, fieldPath) {
        return fieldPath
            .replaceAll('/', '.')
            .split('.')
            .reduce(
                (acc, key) => (acc === undefined || acc === null ? undefined : acc[key]),
                target
            )
    }
    /**
     * @param {string} fieldPath
     * @returns {string}
     */
    normalizeFieldPath(fieldPath) {
        return fieldPath.replaceAll('.', '/')
    }
    /**
     * @param {TTIDValue} docId
     * @param {StoreQuery | undefined} query
     * @param {{ createdAt: number, updatedAt: number }} timestamps
     * @returns {boolean}
     */
    matchesTimestamp(docId, query, timestamps) {
        if (!query?.$created && !query?.$updated) return true
        /**
         * @param {number} value
         * @param {import('../query/types.js').TimestampQuery | undefined} range
         * @returns {boolean}
         */
        const match = (value, range) => {
            if (!range) return true
            if (range.$gt !== undefined && !(value > range.$gt)) return false
            if (range.$gte !== undefined && !(value >= range.$gte)) return false
            if (range.$lt !== undefined && !(value < range.$lt)) return false
            if (range.$lte !== undefined && !(value <= range.$lte)) return false
            return true
        }
        return (
            match(timestamps.createdAt, query.$created) &&
            match(timestamps.updatedAt, query.$updated)
        )
    }
    /**
     * Matches a soft-deleted document while preserving `$updated` for live
     * document semantics and exposing deletion time through `$deleted`.
     * @param {TTIDValue} docId
     * @param {Record<string, any>} doc
     * @param {StoreQuery | undefined} query
     * @param {{ createdAt: number, deletedAt: number }} timestamps
     * @returns {boolean}
     */
    matchesDeletedQuery(docId, doc, query, timestamps) {
        if (query?.$updated) {
            throw new Error('Deleted document queries use $deleted instead of $updated')
        }
        if (
            !this.matchesQuery(docId, doc, query, {
                ...timestamps,
                updatedAt: timestamps.deletedAt
            })
        )
            return false
        const range = query?.$deleted
        if (!range) return true
        if (range.$gt !== undefined && !(timestamps.deletedAt > range.$gt)) return false
        if (range.$gte !== undefined && !(timestamps.deletedAt >= range.$gte)) return false
        if (range.$lt !== undefined && !(timestamps.deletedAt < range.$lt)) return false
        if (range.$lte !== undefined && !(timestamps.deletedAt <= range.$lte)) return false
        return true
    }
    /**
     * @param {unknown} value
     * @param {Operand} operand
     * @returns {boolean}
     */
    matchesOperand(value, operand) {
        if (operand.$eq !== undefined && value != operand.$eq) return false
        if (operand.$ne !== undefined && value == operand.$ne) return false
        if (operand.$gt !== undefined && !(Number(value) > operand.$gt)) return false
        if (operand.$gte !== undefined && !(Number(value) >= operand.$gte)) return false
        if (operand.$lt !== undefined && !(Number(value) < operand.$lt)) return false
        if (operand.$lte !== undefined && !(Number(value) <= operand.$lte)) return false
        if (
            operand.$like !== undefined &&
            (typeof value !== 'string' ||
                !matchesLike(value, operand.$like, { singleCharacterWildcard: false }))
        )
            return false
        if (operand.$contains !== undefined) {
            if (!Array.isArray(value) || !value.some((item) => item == operand.$contains))
                return false
        }
        return true
    }
    /**
     * @param {Set<TTIDValue> | null} current
     * @param {Iterable<TTIDValue>} next
     * @returns {Set<TTIDValue>}
     */
    intersectDocIds(current, next) {
        const nextSet = next instanceof Set ? next : new Set(next)
        if (current === null) return new Set(nextSet)
        const intersection = new Set()
        for (const docId of current) {
            if (nextSet.has(docId)) intersection.add(docId)
        }
        return intersection
    }
    /**
     * Resolves candidate document ids for one field operand using collection indexes.
     * @param {string} collection
     * @param {string} fieldPath
     * @param {Operand} operand
     * @returns {Promise<Set<TTIDValue> | null>}
     */
    async candidateDocIdsForOperand(collection, fieldPath, operand) {
        if (Cipher.isConfigured() && Cipher.isEncryptedField(collection, fieldPath)) {
            const unsupported =
                operand.$ne !== undefined ||
                operand.$gt !== undefined ||
                operand.$gte !== undefined ||
                operand.$lt !== undefined ||
                operand.$lte !== undefined ||
                operand.$like !== undefined ||
                operand.$contains !== undefined
            if (unsupported) {
                throw new Error(`Operator is not supported on encrypted field: ${fieldPath}`)
            }
        }
        let candidateIds = null
        if (operand.$eq !== undefined) {
            candidateIds = this.intersectDocIds(
                candidateIds,
                (await this.context.index.candidateDocIds(collection, fieldPath, {
                    $eq: operand.$eq
                })) ?? new Set()
            )
        }
        if (
            operand.$gt !== undefined ||
            operand.$gte !== undefined ||
            operand.$lt !== undefined ||
            operand.$lte !== undefined
        ) {
            for (const key of /** @type {const} */ (['$gt', '$gte', '$lt', '$lte'])) {
                if (operand[key] === undefined) continue
                const rangeCandidates = await this.context.index.candidateDocIds(
                    collection,
                    fieldPath,
                    { [key]: operand[key] }
                )
                if (rangeCandidates === null) return null
                candidateIds = this.intersectDocIds(candidateIds, rangeCandidates)
            }
        }
        if (operand.$like !== undefined) {
            const likeCandidates = await this.context.index.candidateDocIds(collection, fieldPath, {
                $like: operand.$like
            })
            if (likeCandidates === null) return null
            candidateIds = this.intersectDocIds(candidateIds, likeCandidates)
        }
        if (operand.$contains !== undefined) {
            const containsCandidates = await this.context.index.candidateDocIds(
                collection,
                fieldPath,
                { $contains: operand.$contains }
            )
            candidateIds = this.intersectDocIds(candidateIds, containsCandidates ?? new Set())
        }
        return candidateIds
    }
    /**
     * @param {string} collection
     * @param {QueryOperation} operation
     * @returns {Promise<Set<TTIDValue> | null>}
     */
    async candidateDocIdsForOperation(collection, operation) {
        let candidateIds = null
        for (const [field, operand] of Object.entries(operation)) {
            if (!operand) continue
            const fieldPath = this.normalizeFieldPath(String(field))
            const fieldCandidates = await this.candidateDocIdsForOperand(
                collection,
                fieldPath,
                operand
            )
            if (fieldCandidates === null) continue
            candidateIds = this.intersectDocIds(candidateIds, fieldCandidates)
        }
        return candidateIds
    }
    /**
     * @param {string} collection
     * @param {StoreQuery | undefined} query
     * @returns {Promise<Set<TTIDValue> | null>}
     */
    async candidateDocIdsForQuery(collection, query) {
        if (!query?.$ops || query.$ops.length === 0) return null
        const union = new Set()
        let usedIndex = false
        for (const operation of query.$ops) {
            const candidateIds = await this.candidateDocIdsForOperation(collection, operation)
            if (candidateIds === null) return null
            usedIndex = true
            for (const docId of candidateIds) union.add(docId)
        }
        return usedIndex ? union : null
    }
    /**
     * @param {TTIDValue} docId
     * @param {Record<string, any>} doc
     * @param {StoreQuery | undefined} query
     * @param {{ createdAt: number, updatedAt: number }} timestamps
     * @returns {boolean}
     */
    matchesQuery(docId, doc, query, timestamps) {
        if (!this.matchesTimestamp(docId, query, timestamps)) return false
        if (!query?.$ops || query.$ops.length === 0) return true
        return query.$ops.some((operation) => {
            for (const field in operation) {
                const value = this.getValueByPath(doc, field)
                const operand = operation[field]
                if (!operand || !this.matchesOperand(value, operand)) return false
            }
            return true
        })
    }
    /**
     * @param {string[]} selection
     * @param {Record<string, any>} data
     * @returns {Record<string, any>}
     */
    selectValues(selection, data) {
        const copy = { ...data }
        for (const field in copy) {
            if (!selection.includes(field)) delete copy[field]
        }
        return copy
    }
    /**
     * @param {Record<string, string>} rename
     * @param {Record<string, any>} data
     * @returns {Record<string, any>}
     */
    renameFields(rename, data) {
        const copy = { ...data }
        for (const field in copy) {
            if (rename[field]) {
                copy[rename[field]] = copy[field]
                delete copy[field]
            }
        }
        return copy
    }
    /**
     * Applies projection, renaming, grouping, and only-id shaping to one result record.
     * @param {Record<TTIDValue, Record<string, any>>} doc
     * @param {StoreQuery | undefined} query
     * @returns {TTIDValue | Record<string, any> | undefined}
     */
    processDoc(doc, query) {
        if (Object.keys(doc).length === 0) return
        const next = { ...doc }
        for (let [_id, data] of Object.entries(next)) {
            if (query?.$select?.length) data = this.selectValues(query.$select, data)
            if (query?.$rename) data = this.renameFields(query.$rename, data)
            next[_id] = data
        }
        if (query?.$groupby) {
            /** @type {Record<string, Record<TTIDValue, Record<string, any>>>} */
            const docGroup = {}
            for (const [id, data] of Object.entries(next)) {
                const groupValue = data[query.$groupby]
                if (groupValue) {
                    const groupData = { ...data }
                    delete groupData[query.$groupby]
                    docGroup[groupValue] = { [id]: groupData }
                }
            }
            if (query.$onlyIds) {
                /** @type {Record<string, TTIDValue[]>} */
                const groupedIds = {}
                for (const group in docGroup) groupedIds[group] = Object.keys(docGroup[group])
                return groupedIds
            }
            return docGroup
        }
        if (query?.$onlyIds) return Object.keys(next).shift()
        return next
    }
}
