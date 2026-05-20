/**
 * @fileoverview Top-level RLS engine. Routes a request through the rules
 * file for a collection, selects the first applicable role, and either
 * approves the operation or denies it (FyloAuthError).
 *
 * Reads use a two-phase model:
 *   1. authorizeOperation('doc:read'/'doc:find') → confirms the user has
 *      a role with a `read` permission. (Coarse: "can this user read at
 *      all?")
 *   2. isDocVisible(doc) is then called per emitted doc by the wrapper —
 *      docs that do not satisfy the role's `read.filter` are dropped.
 *
 * Writes are single-phase: the predicate/filter is evaluated against the
 * request data (insert) or the existing doc (update/delete).
 */
import { loadRules } from './loader.js'
import { evaluatePredicate, evaluateFilter, resolveFilter } from './expressions.js'
import { FyloAuthError } from '../auth.js'

/**
 * @typedef {import('./loader.js').FyloRulesFile} FyloRulesFile
 * @typedef {import('./loader.js').FyloRoleRule} FyloRoleRule
 * @typedef {import('../auth.js').FyloAuthAction} FyloAuthAction
 * @typedef {import('../auth.js').FyloAuthContext} FyloAuthContext
 */

/**
 * @param {FyloAuthContext} auth
 * @param {Record<string, unknown> | null | undefined} root
 * @param {Record<string, unknown> | null | undefined} request
 */
function makeContext(auth, root, request) {
    return {
        user: auth,
        root: root ?? {},
        request: request ?? {}
    }
}

/**
 * First role whose `apply_when` matches the context, or null.
 * @param {FyloRulesFile} rules
 * @param {Record<string, any>} context
 * @returns {FyloRoleRule | null}
 */
function selectRole(rules, context) {
    for (const role of rules.roles) {
        const applies = evaluatePredicate(role.apply_when ?? true, context)
        if (applies) return role
    }
    return null
}

/**
 * Evaluates RLS rule files for read, write, collection, bulk, join, and SQL
 * operations.
 */
export class RulesAuthorizer {
    /**
     * @param {AuthorizeArgs} args
     * @returns {Promise<{ role: FyloRoleRule }>}
     */
    async authorizeOperation(args) {
        const { collection, schemaDir, auth, action, docId, data, existing } = args
        const rules = await loadRules(collection, schemaDir)
        if (!rules) throw new FyloAuthError({ auth, action, collection, docId })

        const requestRoot = action === 'doc:create' ? data : (existing ?? data ?? null)
        const context = makeContext(auth, requestRoot ?? null, { action, docId })
        const role = selectRole(rules, context)
        if (!role) throw new FyloAuthError({ auth, action, collection, docId })

        return this.#authorizeRole({ role, auth, action, collection, docId, data, existing })
    }

    /**
     * @param {object} args
     * @param {FyloRoleRule} args.role
     * @param {FyloAuthContext} args.auth
     * @param {FyloAuthAction} args.action
     * @param {string} args.collection
     * @param {string=} args.docId
     * @param {Record<string, unknown>=} args.data
     * @param {Record<string, unknown>=} args.existing
     * @returns {{ role: FyloRoleRule }}
     */
    #authorizeRole(args) {
        const { role, auth, action, collection, docId, data, existing } = args
        switch (action) {
            case 'doc:read':
            case 'doc:find':
                if (!role.read) throw new FyloAuthError({ auth, action, collection, docId })
                return { role }
            case 'doc:create': {
                if (!role.insert) throw new FyloAuthError({ auth, action, collection, docId })
                const context = makeContext(auth, data ?? null, { action, docId })
                if (!evaluatePredicate(role.insert.predicate ?? true, context)) {
                    throw new FyloAuthError({ auth, action, collection, docId })
                }
                return { role }
            }
            case 'doc:update':
                return this.#authorizeUpdate({
                    role,
                    auth,
                    action,
                    collection,
                    docId,
                    data,
                    existing
                })
            case 'doc:delete':
                return this.#authorizeDelete({ role, auth, action, collection, docId, existing })
            default:
                this.#assertActionAllowed(role, { auth, action, collection, docId })
                return { role }
        }
    }

    /**
     * @param {object} args
     * @param {FyloRoleRule} args.role
     * @param {FyloAuthContext} args.auth
     * @param {FyloAuthAction} args.action
     * @param {string} args.collection
     * @param {string=} args.docId
     * @param {Record<string, unknown>=} args.data
     * @param {Record<string, unknown>=} args.existing
     * @returns {{ role: FyloRoleRule }}
     */
    #authorizeUpdate(args) {
        const { role, auth, action, collection, docId, data, existing } = args
        if (!role.update) throw new FyloAuthError({ auth, action, collection, docId })
        const context = makeContext(auth, existing ?? {}, { action, docId })
        if (!evaluateFilter(role.update.filter, existing ?? {}, context)) {
            throw new FyloAuthError({ auth, action, collection, docId })
        }
        const allowedFields = role.update.fields
        if (Array.isArray(allowedFields) && allowedFields.length > 0) {
            for (const field of Object.keys(data ?? {})) {
                if (!allowedFields.includes(field)) {
                    throw new FyloAuthError({ auth, action, collection, docId })
                }
            }
        }
        return { role }
    }

    /**
     * @param {object} args
     * @param {FyloRoleRule} args.role
     * @param {FyloAuthContext} args.auth
     * @param {FyloAuthAction} args.action
     * @param {string} args.collection
     * @param {string=} args.docId
     * @param {Record<string, unknown>=} args.existing
     * @returns {{ role: FyloRoleRule }}
     */
    #authorizeDelete(args) {
        const { role, auth, action, collection, docId, existing } = args
        if (!role.delete) throw new FyloAuthError({ auth, action, collection, docId })
        const context = makeContext(auth, existing ?? {}, { action, docId })
        if (!evaluateFilter(role.delete.filter, existing ?? {}, context)) {
            throw new FyloAuthError({ auth, action, collection, docId })
        }
        return { role }
    }

    /**
     * @param {FyloRoleRule} role
     * @param {{ auth: FyloAuthContext, action: FyloAuthAction, collection: string, docId?: string }} denial
     */
    #assertActionAllowed(role, denial) {
        if (!(role.allow_actions ?? []).includes(denial.action)) throw new FyloAuthError(denial)
    }

    /**
     * @param {object} args
     * @param {string} args.collection
     * @param {string|null|undefined} args.schemaDir
     * @param {FyloAuthContext} args.auth
     * @param {Record<string, unknown>} args.doc
     * @returns {Promise<boolean>}
     */
    async isDocVisible(args) {
        const { collection, schemaDir, auth, doc } = args
        const rules = await loadRules(collection, schemaDir)
        if (!rules) return false
        const context = makeContext(auth, doc, {})
        const role = selectRole(rules, context)
        if (!role || !role.read) return false
        return evaluateFilter(role.read.filter, doc, context)
    }

    /**
     * @param {object} args
     * @param {string} args.collection
     * @param {string|null|undefined} args.schemaDir
     * @param {FyloAuthContext} args.auth
     * @returns {Promise<Record<string, unknown> | null>}
     */
    async effectiveReadFilter(args) {
        const { collection, schemaDir, auth } = args
        const rules = await loadRules(collection, schemaDir)
        if (!rules) return null
        const context = makeContext(auth, null, {})
        const role = selectRole(rules, context)
        if (!role || !role.read) return null
        return resolveFilter(role.read.filter, context)
    }
}

/** Shared process-level RLS authorizer. */
export const rulesAuthorizer = new RulesAuthorizer()

/**
 * @typedef {object} AuthorizeArgs
 * @property {string} collection
 * @property {string|null|undefined} schemaDir
 * @property {FyloAuthContext} auth
 * @property {FyloAuthAction} action
 * @property {string=} docId
 * @property {Record<string, unknown>=} data            // for insert/update: the new data
 * @property {Record<string, unknown>=} existing        // for update/delete: the doc on disk
 */

/**
 * Authorize a single operation. Throws FyloAuthError on deny.
 * @param {AuthorizeArgs} args
 * @returns {Promise<{ role: FyloRoleRule }>}
 */
export async function authorizeOperation(args) {
    return await rulesAuthorizer.authorizeOperation(args)
}

/**
 * After authorize-find/read, this is called per emitted doc to drop those
 * the user is not allowed to see. Returns false → caller skips the doc.
 *
 * @param {object} args
 * @param {string} args.collection
 * @param {string|null|undefined} args.schemaDir
 * @param {FyloAuthContext} args.auth
 * @param {Record<string, unknown>} args.doc
 * @returns {Promise<boolean>}
 */
export async function isDocVisible(args) {
    return await rulesAuthorizer.isDocVisible(args)
}

/**
 * Resolve a role's read filter to a concrete (substituted) filter object.
 * Useful for callers that want to inspect or merge the filter rather than
 * post-filter doc-by-doc. Returns null when the user has no read role.
 *
 * @param {object} args
 * @param {string} args.collection
 * @param {string|null|undefined} args.schemaDir
 * @param {FyloAuthContext} args.auth
 * @returns {Promise<Record<string, unknown> | null>}
 */
export async function effectiveReadFilter(args) {
    return await rulesAuthorizer.effectiveReadFilter(args)
}
