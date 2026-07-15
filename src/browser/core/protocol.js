import '../../core/extensions.js'
import { safeRecord } from '../../query/safe-record.js'

export const FYLO_BROWSER_PROTOCOL_VERSION = 1

/**
 * @typedef {import('./types.js').BrowserRequest} BrowserRequest
 * @typedef {import('./types.js').BrowserOperation} BrowserOperation
 * @typedef {import('./types.js').BrowserSuccessResponse} BrowserSuccessResponse
 * @typedef {import('./types.js').BrowserErrorResponse} BrowserErrorResponse
 */

/**
 * @param {unknown} value
 * @returns {value is Record<string, any>}
 */
export function isRecord(value) {
    return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * @param {BrowserRequest} request
 * @param {keyof BrowserRequest} field
 * @returns {string}
 */
export function requireString(request, field) {
    const value = request[field]
    if (typeof value !== 'string' || value.trim().length === 0) {
        throw new Error(`FYLO browser request field "${String(field)}" must be a non-empty string`)
    }
    return value
}

/**
 * @param {BrowserRequest} request
 * @param {keyof BrowserRequest} field
 * @returns {Record<string, any>}
 */
export function requireObject(request, field) {
    const value = request[field]
    if (!isRecord(value)) {
        throw new Error(`FYLO browser request field "${String(field)}" must be an object`)
    }
    return value
}

/**
 * @param {BrowserRequest} request
 * @param {keyof BrowserRequest} field
 * @returns {Record<string, any>[]}
 */
export function requireObjectArray(request, field) {
    const value = request[field]
    if (!Array.isArray(value) || value.some((item) => !isRecord(item))) {
        throw new Error(`FYLO browser request field "${String(field)}" must be an array of objects`)
    }
    return value
}

/**
 * @param {unknown} value
 * @returns {value is BrowserRequest}
 */
export function isBrowserRequest(value) {
    return isRecord(value) && typeof value.op === 'string'
}

/**
 * @param {any} fylo
 * @param {string} collection
 * @param {Record<string, any>} query
 * @returns {Promise<Record<string, any> | string[]>}
 */
async function collectFindDocs(fylo, collection, query) {
    /** @type {Record<string, any> | string[]} */
    let docs = query.$onlyIds ? [] : safeRecord()
    for await (const value of fylo.findDocs(collection, query).collect()) {
        if (value === undefined) continue
        if (typeof value === 'object' && value !== null) {
            docs = /** @type {{ appendGroup(target: any, value: any): any }} */ (
                /** @type {unknown} */ (Object)
            ).appendGroup(docs, value)
            continue
        }
        if (Array.isArray(docs)) docs.push(String(value))
    }
    return docs
}

/**
 * @param {any} fylo
 * @param {string} collection
 * @param {Record<string, any>} query
 * @returns {Promise<Record<string, any> | string[]>}
 */
async function collectDeletedDocs(fylo, collection, query) {
    /** @type {Record<string, any> | string[]} */
    let docs = query.$onlyIds ? [] : safeRecord()
    for await (const value of fylo.findDeletedDocs(collection, query).collect()) {
        if (value === undefined) continue
        if (typeof value === 'object' && value !== null) {
            docs = /** @type {{ appendGroup(target: any, value: any): any }} */ (
                /** @type {unknown} */ (Object)
            ).appendGroup(docs, value)
            continue
        }
        if (Array.isArray(docs)) docs.push(String(value))
    }
    return docs
}

/**
 * Executes one protocol operation against any object implementing FYLO's
 * browser-safe method surface.
 *
 * @param {any} fylo
 * @param {BrowserRequest} request
 * @returns {Promise<unknown>}
 */
export async function executeBrowserOperation(fylo, request) {
    if (!isBrowserRequest(request)) throw new Error('FYLO browser request body must be an object')
    switch (request.op) {
        case 'executeSQL':
            return await fylo.executeSQL(requireString(request, 'sql'))
        case 'createCollection': {
            const collection = requireString(request, 'collection')
            await fylo.createCollection(collection)
            return { collection }
        }
        case 'dropCollection': {
            const collection = requireString(request, 'collection')
            await fylo.dropCollection(collection)
            return { collection }
        }
        case 'inspectCollection':
            return await fylo.inspectCollection(requireString(request, 'collection'))
        case 'rebuildCollection':
            return await fylo.rebuildCollection(requireString(request, 'collection'))
        case 'getDoc':
            return await fylo
                .getDoc(
                    requireString(request, 'collection'),
                    requireString(request, 'id'),
                    request.onlyId === true
                )
                .once()
        case 'getLatest':
            return await fylo.getLatest(
                requireString(request, 'collection'),
                requireString(request, 'id'),
                request.onlyId === true
            )
        case 'getMeta':
            return await fylo.getDocMeta(
                requireString(request, 'collection'),
                requireString(request, 'id')
            )
        case 'setMeta':
            return await fylo.setDocMetaRecord(
                requireString(request, 'collection'),
                requireString(request, 'id'),
                requireObject(request, 'meta')
            )
        case 'findDocs':
            return await collectFindDocs(
                fylo,
                requireString(request, 'collection'),
                isRecord(request.query) ? request.query : {}
            )
        case 'findDeletedDocs':
            return await collectDeletedDocs(
                fylo,
                requireString(request, 'collection'),
                isRecord(request.query) ? request.query : {}
            )
        case 'joinDocs':
            return await fylo.join(requireObject(request, 'join'))
        case 'putData': {
            const hasMeta = Object.hasOwn(request, 'meta')
            return await fylo.putData(
                requireString(request, 'collection'),
                requireObject(request, 'data'),
                hasMeta ? requireObject(request, 'meta') : undefined,
                hasMeta
            )
        }
        case 'batchPutData':
            return await fylo.batchPutData(
                requireString(request, 'collection'),
                requireObjectArray(request, 'batch')
            )
        case 'patchDoc':
            return await fylo.patchDoc(
                requireString(request, 'collection'),
                requireObject(request, 'newDoc'),
                isRecord(request.oldDoc) ? request.oldDoc : {}
            )
        case 'patchDocs':
            return await fylo.patchDocs(
                requireString(request, 'collection'),
                requireObject(request, 'update')
            )
        case 'delDoc':
            await fylo.delDoc(requireString(request, 'collection'), requireString(request, 'id'))
            return { deleted: true }
        case 'restoreDoc': {
            const id = await fylo.restoreDoc(
                requireString(request, 'collection'),
                requireString(request, 'id')
            )
            return { restored: true, id }
        }
        case 'delDocs':
            return await fylo.delDocs(
                requireString(request, 'collection'),
                requireObject(request, 'delete')
            )
        default:
            throw new Error(`Unsupported FYLO browser operation: ${request.op}`)
    }
}

/**
 * @param {any} fylo
 * @param {unknown} request
 * @returns {Promise<BrowserSuccessResponse | BrowserErrorResponse>}
 */
export async function runBrowserRequest(fylo, request) {
    const startedAt = Date.now()
    const safeRequest = isRecord(request) ? /** @type {Partial<BrowserRequest>} */ (request) : {}
    try {
        const result = await executeBrowserOperation(fylo, /** @type {BrowserRequest} */ (request))
        return {
            protocolVersion: FYLO_BROWSER_PROTOCOL_VERSION,
            ok: true,
            op: /** @type {BrowserOperation} */ (safeRequest.op),
            requestId: typeof safeRequest.requestId === 'string' ? safeRequest.requestId : null,
            durationMs: Date.now() - startedAt,
            result
        }
    } catch (error) {
        const failure = /** @type {Error & { code?: string }} */ (error)
        return {
            protocolVersion: FYLO_BROWSER_PROTOCOL_VERSION,
            ok: false,
            op:
                typeof safeRequest.op === 'string'
                    ? /** @type {BrowserOperation} */ (safeRequest.op)
                    : null,
            requestId: typeof safeRequest.requestId === 'string' ? safeRequest.requestId : null,
            durationMs: Date.now() - startedAt,
            error: {
                name: failure.name || 'Error',
                message: failure.message || 'Unknown error',
                ...(typeof failure.code === 'string' ? { code: failure.code } : {})
            }
        }
    }
}
