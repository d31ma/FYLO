import path from 'node:path'
import Fylo from '../index.js'
import { runMachineRequest } from '../cli/machine.js'
import { validateBranchName, VersionRepository } from '../versioning/repository.js'
import { validateCollectionName } from '../core/collection.js'
import { validateDocId } from '../core/doc-id.js'

const DEFAULT_MAX_BODY_BYTES = 1_048_576
const FYLO_REMOTE_PROTOCOL_VERSION = 1

/**
 * @typedef {object} FyloHttpServerOptions
 * @property {string} root
 * @property {string=} host
 * @property {number=} port
 * @property {string=} token
 * @property {string | string[]=} corsOrigin
 * @property {number=} maxBodyBytes
 * @property {boolean=} allowAnonymous
 */

/**
 * @typedef {object} FyloHttpContext
 * @property {URL} url
 * @property {Request} request
 * @property {string} root
 * @property {string | undefined} branch
 * @property {FyloHttpServerOptions & { root: string, maxBodyBytes: number }} options
 */

/**
 * Creates the fetch handler used by both `fylo serve` and integration tests.
 * @param {FyloHttpServerOptions} options
 * @returns {(request: Request) => Promise<Response>}
 */
export function createFyloHttpHandler(options) {
    const normalized = normalizeServerOptions(options)
    return async (request) => {
        const url = new URL(request.url)
        try {
            if (request.method === 'OPTIONS') return emptyResponse(204, normalized)
            if (!isAuthorized(request, normalized)) {
                return jsonResponse(
                    { ok: false, error: { message: 'Unauthorized' } },
                    401,
                    normalized
                )
            }
            const branch = selectBranch(request)
            const context = { url, request, root: normalized.root, branch, options: normalized }
            if (url.pathname === '/v1/health' && request.method === 'GET') {
                return jsonResponse(
                    {
                        ok: true,
                        protocolVersion: FYLO_REMOTE_PROTOCOL_VERSION,
                        root: normalized.root,
                        branch: branch ?? null
                    },
                    200,
                    normalized
                )
            }
            if (url.pathname === '/v1/openapi.json' && request.method === 'GET') {
                return jsonResponse(openApiDocument(), 200, normalized)
            }
            if (url.pathname === '/v1/exec' && request.method === 'POST') {
                const body = await readJsonBody(request, normalized.maxBodyBytes)
                const response = await runMachineRequest(body, await machineOverridesFor(context))
                return jsonResponse(response, response.ok ? 200 : 400, normalized)
            }
            if (url.pathname === '/v1/sql' && request.method === 'POST') {
                const body = await readJsonBody(request, normalized.maxBodyBytes)
                if (!isRecord(body) || typeof body.sql !== 'string') {
                    throw statusError(400, 'SQL request body must include a string "sql" field')
                }
                const result = await (await fyloFor(context))._sql(body.sql)
                return jsonResponse({ ok: true, result }, 200, normalized)
            }
            const route = parseCollectionRoute(url.pathname)
            if (route) return await handleCollectionRoute(context, route)
            return jsonResponse({ ok: false, error: { message: 'Not found' } }, 404, normalized)
        } catch (error) {
            const failure = /** @type {Error & { status?: number }} */ (error)
            return jsonResponse(
                {
                    ok: false,
                    error: {
                        name: failure.name || 'Error',
                        message: failure.message || 'Unknown error'
                    }
                },
                failure.status ?? 500,
                normalized
            )
        }
    }
}

/**
 * Starts a Bun HTTP server for FYLO remote access.
 * @param {FyloHttpServerOptions} options
 * @returns {ReturnType<typeof Bun.serve>}
 */
export function serveFyloHttp(options) {
    const normalized = normalizeServerOptions(options)
    assertSafeServeConfig(normalized)
    return Bun.serve({
        hostname: normalized.host ?? '127.0.0.1',
        port: normalized.port ?? 8787,
        fetch: createFyloHttpHandler(normalized)
    })
}

/**
 * @param {FyloHttpContext} context
 * @param {{ collection: string, id?: string }} route
 * @returns {Promise<Response>}
 */
async function handleCollectionRoute(context, route) {
    validateCollectionName(route.collection)
    if (route.id) validateDocId(route.id)
    const fylo = await fyloFor(context)
    const options = context.options
    if (!route.id && context.request.method === 'GET') {
        const query = queryFromUrl(context.url)
        const result = await collectFindDocs(fylo, route.collection, query)
        return jsonResponse({ ok: true, result }, 200, options)
    }
    if (!route.id && context.request.method === 'POST') {
        const body = await readJsonBody(context.request, options.maxBodyBytes)
        if (!isRecord(body)) throw statusError(400, 'Document body must be a JSON object')
        const id = await fylo[route.collection].put(body)
        return jsonResponse({ ok: true, result: { id } }, 201, options)
    }
    if (route.id && context.request.method === 'GET') {
        const result = await fylo[route.collection].get(route.id).once()
        if (Object.keys(result).length === 0) {
            return jsonResponse(
                { ok: false, error: { message: 'Document not found' } },
                404,
                options
            )
        }
        return jsonResponse({ ok: true, result }, 200, options)
    }
    if (route.id && context.request.method === 'PATCH') {
        const current = await fylo[route.collection].get(route.id).once()
        if (Object.keys(current).length === 0) {
            return jsonResponse(
                { ok: false, error: { message: 'Document not found' } },
                404,
                options
            )
        }
        const body = await readJsonBody(context.request, options.maxBodyBytes)
        if (!isRecord(body)) throw statusError(400, 'Patch body must be a JSON object')
        const id = await fylo[route.collection].patch(route.id, body)
        return jsonResponse({ ok: true, result: { id } }, 200, options)
    }
    if (route.id && context.request.method === 'DELETE') {
        await fylo[route.collection].delete(route.id)
        return jsonResponse({ ok: true, result: { deleted: true, id: route.id } }, 200, options)
    }
    return jsonResponse({ ok: false, error: { message: 'Method not allowed' } }, 405, options)
}

/**
 * @param {FyloHttpContext} context
 * @returns {Promise<import('../api/fylo.js').FyloCollections>}
 */
async function fyloFor(context) {
    return /** @type {import('../api/fylo.js').FyloCollections} */ (
        /** @type {unknown} */ (
            new Fylo(await resolveRoot(context), {
                ...(context.branch ? { versioning: { resolve: false } } : {})
            })
        )
    )
}

/**
 * @param {FyloHttpContext} context
 * @returns {Promise<string>}
 */
async function resolveRoot(context) {
    if (!context.branch) return context.root
    const repository = new VersionRepository(context.root)
    await repository.readRef(context.branch)
    return repository.branchRoot(context.branch)
}

/**
 * @param {FyloHttpContext} context
 * @returns {Promise<{ root: string, versioning?: { resolve?: boolean }}>}
 */
async function machineOverridesFor(context) {
    return {
        root: await resolveRoot(context),
        ...(context.branch ? { versioning: { resolve: false } } : {})
    }
}

/**
 * @param {import('../api/fylo.js').FyloCollections} fylo
 * @param {string} collection
 * @param {Record<string, any>} query
 * @returns {Promise<Record<string, any> | string[]>}
 */
async function collectFindDocs(fylo, collection, query) {
    /** @type {Record<string, any> | string[]} */
    let docs = query.$onlyIds ? [] : {}
    for await (const value of fylo[collection].find(query).collect()) {
        if (value === undefined) continue
        if (typeof value === 'object' && value !== null) {
            docs = /** @type {{ appendGroup(target: any, value: any): any }} */ (
                /** @type {unknown} */ (Object)
            ).appendGroup(docs, value)
        } else if (Array.isArray(docs)) {
            docs.push(String(value))
        }
    }
    return docs
}

/**
 * @param {string} pathname
 * @returns {{ collection: string, id?: string } | null}
 */
function parseCollectionRoute(pathname) {
    const parts = pathname.split('/').filter(Boolean)
    if (parts[0] !== 'v1' || parts.length < 2 || parts.length > 3) return null
    const collection = decodeURIComponent(parts[1])
    if (['health', 'openapi.json', 'exec', 'sql'].includes(collection)) return null
    return { collection, ...(parts[2] ? { id: decodeURIComponent(parts[2]) } : {}) }
}

/**
 * @param {URL} url
 * @returns {Record<string, any>}
 */
function queryFromUrl(url) {
    /** @type {Record<string, any>} */
    const query = {}
    /** @type {Record<string, any>[]} */
    const ops = []
    for (const [key, value] of url.searchParams) {
        if (key === 'limit') {
            const limit = Number(value)
            if (!Number.isInteger(limit) || limit < 0) throw statusError(400, 'Invalid limit')
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
        if (operand) ops.push({ [key]: operand })
    }
    if (ops.length > 0) query.$ops = ops
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

/**
 * @param {Request} request
 * @param {number} maxBytes
 * @returns {Promise<unknown>}
 */
async function readJsonBody(request, maxBytes) {
    const contentLength = request.headers.get('content-length')
    if (contentLength && Number(contentLength) > maxBytes) {
        throw statusError(413, `Request body exceeds ${maxBytes} bytes`)
    }
    const text = await request.text()
    const bytes = new TextEncoder().encode(text).byteLength
    if (bytes > maxBytes) throw statusError(413, `Request body exceeds ${maxBytes} bytes`)
    if (!text.trim()) throw statusError(400, 'Request body is empty')
    try {
        return JSON.parse(text)
    } catch {
        throw statusError(400, 'Request body must be valid JSON')
    }
}

/**
 * @param {Request} request
 * @param {FyloHttpServerOptions} options
 * @returns {boolean}
 */
function isAuthorized(request, options) {
    if (request.method === 'OPTIONS') return true
    if (!options.token) return options.allowAnonymous === true
    return request.headers.get('authorization') === `Bearer ${options.token}`
}

/**
 * @param {Request} request
 * @returns {string | undefined}
 */
function selectBranch(request) {
    const profile =
        request.method === 'GET' || request.method === 'HEAD'
            ? request.headers.get('accept-profile')
            : request.headers.get('content-profile') || request.headers.get('accept-profile')
    if (!profile) return undefined
    validateBranchName(profile)
    return profile
}

/**
 * @param {FyloHttpServerOptions} options
 * @returns {FyloHttpServerOptions & { root: string, maxBodyBytes: number }}
 */
function normalizeServerOptions(options) {
    return {
        ...options,
        root: path.resolve(options.root || Fylo.defaultRoot()),
        host: options.host ?? '127.0.0.1',
        port: options.port ?? 8787,
        token: options.token ?? process.env.FYLO_SERVER_TOKEN,
        maxBodyBytes: options.maxBodyBytes ?? DEFAULT_MAX_BODY_BYTES
    }
}

/**
 * @param {FyloHttpServerOptions} options
 * @returns {void}
 */
function assertSafeServeConfig(options) {
    if (!options.token && options.allowAnonymous !== true && !isLoopbackHost(options.host)) {
        throw new Error(
            'fylo serve requires --token or FYLO_SERVER_TOKEN when binding non-loopback hosts'
        )
    }
}

/**
 * @param {string | undefined} host
 * @returns {boolean}
 */
function isLoopbackHost(host) {
    return !host || ['127.0.0.1', 'localhost', '::1'].includes(host)
}

/**
 * @param {unknown} value
 * @returns {value is Record<string, any>}
 */
function isRecord(value) {
    return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * @param {number} status
 * @param {string} message
 * @returns {Error & { status: number }}
 */
function statusError(status, message) {
    const error = /** @type {Error & { status: number }} */ (new Error(message))
    error.status = status
    return error
}

/**
 * @param {unknown} body
 * @param {number} status
 * @param {FyloHttpServerOptions} options
 * @returns {Response}
 */
function jsonResponse(body, status, options) {
    return new Response(JSON.stringify(body), {
        status,
        headers: responseHeaders(options, 'application/json')
    })
}

/**
 * @param {number} status
 * @param {FyloHttpServerOptions} options
 * @returns {Response}
 */
function emptyResponse(status, options) {
    return new Response(null, { status, headers: responseHeaders(options) })
}

/**
 * @param {FyloHttpServerOptions} options
 * @param {string=} contentType
 * @returns {Headers}
 */
function responseHeaders(options, contentType) {
    const headers = new Headers()
    if (contentType) headers.set('content-type', contentType)
    headers.set('vary', 'origin, accept-profile, content-profile')
    if (options.corsOrigin) {
        const origin = Array.isArray(options.corsOrigin)
            ? options.corsOrigin.join(', ')
            : options.corsOrigin
        headers.set('access-control-allow-origin', origin)
        headers.set('access-control-allow-methods', 'GET,HEAD,POST,PATCH,DELETE,OPTIONS')
        headers.set(
            'access-control-allow-headers',
            'authorization,content-type,accept-profile,content-profile'
        )
    }
    return headers
}

/**
 * @returns {Record<string, any>}
 */
function openApiDocument() {
    return {
        openapi: '3.1.0',
        info: {
            title: 'FYLO Remote API',
            version: String(FYLO_REMOTE_PROTOCOL_VERSION)
        },
        paths: {
            '/v1/health': { get: { summary: 'Health check' } },
            '/v1/exec': { post: { summary: 'Execute FYLO machine JSON request' } },
            '/v1/sql': { post: { summary: 'Execute FYLO SQL' } },
            '/v1/{collection}': {
                get: { summary: 'Query collection documents' },
                post: { summary: 'Create a document' }
            },
            '/v1/{collection}/{id}': {
                get: { summary: 'Read a document by TTID' },
                patch: { summary: 'Patch a document by TTID' },
                delete: { summary: 'Soft-delete a document by TTID' }
            }
        }
    }
}
