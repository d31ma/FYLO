/**
 * @fileoverview Rules-file loader. Reads collection-local rules first, then
 * optional shared rules from `<schemaDir>/rules.json`, validates shape, and
 * caches per process.
 *
 * Rules file format:
 * {
 *   "version": 1,                              // schema-of-schema for forward-compat
 *   "roles": [
 *     {
 *       "name": "admin",
 *       "apply_when": { "$in": ["admin", "%%user.roles"] },
 *       "read":   { "filter": {} },
 *       "insert": { "predicate": true },
 *       "update": { "filter": {}, "fields": null },   // null|undefined → all fields allowed
 *       "delete": { "filter": {} },
 *       "allow_actions": ["sql:execute", "bulk:export", "collection:create"]
 *     }
 *   ]
 * }
 *
 * Roles are evaluated in declaration order; the first whose `apply_when`
 * matches wins (Atlas Rules behaviour).
 */
import path from 'node:path'

/**
 * @typedef {object} FyloRolePerm
 * @property {Record<string, unknown>=} filter
 * @property {string[] | null=} fields    // null/undefined/[] all mean "no field restriction"
 * @property {unknown=} predicate
 *
 * @typedef {object} FyloRoleRule
 * @property {string} name
 * @property {unknown=} apply_when
 * @property {FyloRolePerm=} read
 * @property {FyloRolePerm=} insert
 * @property {FyloRolePerm=} update
 * @property {FyloRolePerm=} delete
 * @property {string[]=} allow_actions
 *
 * @typedef {object} FyloRulesFile
 * @property {number=} version
 * @property {FyloRoleRule[]} roles
 *
 * @typedef {Record<string, FyloRulesFile | Record<string, FyloRulesFile>>} FyloSharedRulesIndex
 */

/**
 * @param {string} collection
 * @returns {[string, string] | null}
 */
function splitSharedRuleKey(collection) {
    const dash = collection.indexOf('-')
    if (dash > 0 && dash < collection.length - 1) {
        return [collection.slice(0, dash), collection.slice(dash + 1)]
    }
    return null
}

/**
 * @param {unknown} value
 * @returns {value is FyloRulesFile}
 */
function isRulesFile(value) {
    return (
        !!value &&
        typeof value === 'object' &&
        Array.isArray(/** @type {FyloRulesFile} */ (value).roles)
    )
}

/**
 * @param {FyloSharedRulesIndex} sharedRules
 * @param {string} collection
 * @returns {FyloRulesFile | null}
 */
function selectSharedRules(sharedRules, collection) {
    const direct = sharedRules[collection]
    if (isRulesFile(direct)) return direct
    const split = splitSharedRuleKey(collection)
    if (!split) return null
    const [namespace, name] = split
    const namespaceRules = sharedRules[namespace]
    if (!namespaceRules || typeof namespaceRules !== 'object' || isRulesFile(namespaceRules)) {
        return null
    }
    const nested = namespaceRules[name]
    return isRulesFile(nested) ? nested : null
}

/**
 * Loads and validates collection-specific and shared RLS rules from a schema
 * directory.
 */
export class RulesLoader {
    /** @type {Map<string, FyloRulesFile | null>} */
    cache = new Map()

    /**
     * @param {string} collection
     * @param {string|null|undefined} schemaDir
     * @returns {Promise<FyloRulesFile|null>}
     */
    async load(collection, schemaDir) {
        if (!schemaDir) return null
        const key = `${path.resolve(schemaDir)}\0${collection}`
        if (this.cache.has(key)) return this.cache.get(key) ?? null
        const collectionRulesFile = Bun.file(path.join(schemaDir, collection, 'rules.json'))
        if (await collectionRulesFile.exists()) {
            const rules = /** @type {FyloRulesFile} */ (await collectionRulesFile.json())
            validateRulesShape(rules, collection)
            this.cache.set(key, rules)
            return rules
        }
        const sharedRulesFile = Bun.file(path.join(schemaDir, 'rules.json'))
        if (await sharedRulesFile.exists()) {
            const sharedRules = /** @type {FyloSharedRulesIndex} */ (await sharedRulesFile.json())
            const rules = selectSharedRules(sharedRules, collection)
            if (rules) {
                validateRulesShape(rules, collection)
                this.cache.set(key, rules)
                return rules
            }
        }
        this.cache.set(key, null)
        return null
    }

    /** Clear cached rules files. */
    clearCache() {
        this.cache.clear()
    }
}

/** Shared process-level RLS rules loader. */
export const rulesLoader = new RulesLoader()

/**
 * @param {string} collection
 * @param {string|null|undefined} schemaDir
 * @returns {Promise<FyloRulesFile|null>}
 */
export async function loadRules(collection, schemaDir) {
    return await rulesLoader.load(collection, schemaDir)
}

/** @param {FyloRulesFile} rules @param {string} collection */
function validateRulesShape(rules, collection) {
    if (!rules || typeof rules !== 'object') {
        throw new Error(`Rules file for '${collection}' must be a JSON object`)
    }
    if (!Array.isArray(rules.roles)) {
        throw new Error(`Rules file for '${collection}' must have a 'roles' array`)
    }
    for (const [i, role] of rules.roles.entries()) {
        if (!role || typeof role !== 'object') {
            throw new Error(`Rules role at index ${i} for '${collection}' must be an object`)
        }
        if (typeof role.name !== 'string' || role.name.length === 0) {
            throw new Error(`Rules role at index ${i} for '${collection}' missing 'name'`)
        }
    }
}

/** Test/dev hook to reset memoised rules. */
export function _resetRulesCache() {
    rulesLoader.clearCache()
}
