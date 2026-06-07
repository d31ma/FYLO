import path from 'node:path'
import Fylo from '../index.js'
import {
    doctorSchema,
    inspectSchema,
    materializeSchemaDocument,
    validateSchemaDocument
} from '../schema/admin.js'
import { VersionRepository } from '../versioning/repository.js'

const MACHINE_PROTOCOL_VERSION = 1

/**
 * @typedef {import('../replication/sync.js').FyloWormOptions} FyloWormOptions
 */

/**
 * @typedef {'executeSQL' | 'createCollection' | 'dropCollection' | 'inspectCollection' | 'rebuildCollection' | 'getDoc' | 'getLatest' | 'findDocs' | 'findDeletedDocs' | 'restoreDoc' | 'joinDocs' | 'putData' | 'batchPutData' | 'patchDoc' | 'patchDocs' | 'delDoc' | 'delDocs' | 'importBulkData' | 'checkout' | 'branch' | 'commit' | 'log' | 'status' | 'diff' | 'restoreCommit' | 'merge' | 'schemaInspect' | 'schemaCurrent' | 'schemaHistory' | 'schemaDoctor' | 'schemaValidate' | 'schemaMaterialize'} MachineOperation
 */

/**
 * @typedef {object} MachineRequest
 * @property {MachineOperation} op
 * @property {string=} requestId
 * @property {string=} root
 * @property {string=} schemaDir
 * @property {boolean | FyloWormOptions=} worm
 * @property {string=} collection
 * @property {string=} branch
 * @property {boolean=} create
 * @property {boolean=} force
 * @property {string=} message
 * @property {string=} source
 * @property {string=} from
 * @property {string=} to
 * @property {string=} id
 * @property {boolean=} onlyId
 * @property {string=} sql
 * @property {Record<string, any>=} query
 * @property {Record<string, any>=} join
 * @property {Record<string, any>=} document
 * @property {Record<string, any>=} data
 * @property {Record<string, any>[]=} batch
 * @property {Record<string, any>=} newDoc
 * @property {Record<string, any>=} oldDoc
 * @property {Record<string, any>=} update
 * @property {Record<string, any>=} delete
 * @property {{ wait?: boolean }=} options
 * @property {string=} url
 * @property {number | Record<string, any>=} limitOrOptions
 */

/**
 * @typedef {object} MachineCliOverrides
 * @property {string=} root
 * @property {boolean=} worm
 * @property {{ resolve?: boolean }=} versioning
 */

/**
 * @typedef {object} MachineSuccessResponse
 * @property {number} protocolVersion
 * @property {true} ok
 * @property {MachineOperation} op
 * @property {string | null} requestId
 * @property {number} durationMs
 * @property {unknown} result
 */

/**
 * @typedef {object} MachineErrorResponse
 * @property {number} protocolVersion
 * @property {false} ok
 * @property {MachineOperation | null} op
 * @property {string | null} requestId
 * @property {number} durationMs
 * @property {{ name: string, message: string, code?: string }} error
 */

/**
 * @param {unknown} value
 * @returns {value is Record<string, any>}
 */
function isRecord(value) {
    return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * @param {MachineRequest} request
 * @param {keyof MachineRequest} field
 * @returns {string}
 */
function requireString(request, field) {
    const value = request[field]
    if (typeof value !== 'string' || value.trim().length === 0) {
        throw new Error(`Machine request field "${String(field)}" must be a non-empty string`)
    }
    return value
}

/**
 * @param {MachineRequest} request
 * @param {keyof MachineRequest} field
 * @returns {Record<string, any>}
 */
function requireObject(request, field) {
    const value = request[field]
    if (!isRecord(value)) {
        throw new Error(`Machine request field "${String(field)}" must be an object`)
    }
    return value
}

/**
 * @param {MachineRequest} request
 * @param {keyof MachineRequest} field
 * @returns {Record<string, any>[]}
 */
function requireObjectArray(request, field) {
    const value = request[field]
    if (!Array.isArray(value) || value.some((item) => !isRecord(item))) {
        throw new Error(`Machine request field "${String(field)}" must be an array of objects`)
    }
    return value
}

/**
 * @param {boolean | FyloWormOptions | undefined} worm
 * @returns {FyloWormOptions | undefined}
 */
function normalizeWormOptions(worm) {
    if (worm === undefined || worm === false) return undefined
    if (worm === true) return { mode: 'strict' }
    if (!isRecord(worm)) {
        throw new Error('Machine request field "worm" must be a boolean or object')
    }
    const mode = worm.mode ?? 'strict'
    if (mode !== 'off' && mode !== 'strict') {
        throw new Error('Machine request field "worm.mode" must be "off" or "strict"')
    }
    return { mode }
}

/**
 * @param {MachineRequest} request
 * @param {MachineCliOverrides=} overrides
 * @returns {import('../api/fylo.js').FyloCollections}
 */
function createMachineFylo(request, overrides = {}) {
    const root = overrides.root ?? request.root
    const worm =
        overrides.worm === true
            ? /** @type {FyloWormOptions} */ ({ mode: 'strict' })
            : normalizeWormOptions(request.worm)
    return /** @type {import('../api/fylo.js').FyloCollections} */ (
        /** @type {unknown} */ (
            new Fylo(path.resolve(root ?? Fylo.defaultRoot()), {
                ...(worm ? { worm } : {}),
                ...(overrides.versioning ? { versioning: overrides.versioning } : {})
            })
        )
    )
}

/**
 * @param {MachineRequest} request
 * @param {MachineCliOverrides=} overrides
 * @returns {VersionRepository}
 */
function createMachineRepository(request, overrides = {}) {
    return new VersionRepository(path.resolve(overrides.root ?? request.root ?? Fylo.defaultRoot()))
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
            continue
        }
        if (Array.isArray(docs)) docs.push(String(value))
    }
    return docs
}

/**
 * @param {import('../api/fylo.js').FyloCollections} fylo
 * @param {string} collection
 * @param {Record<string, any>} query
 * @returns {Promise<Record<string, any> | string[]>}
 */
async function collectDeletedDocs(fylo, collection, query) {
    /** @type {Record<string, any> | string[]} */
    let docs = query.$onlyIds ? [] : {}
    for await (const value of fylo[collection].findDeleted(query).collect()) {
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
 * @param {MachineRequest} request
 * @param {MachineCliOverrides=} overrides
 * @returns {Promise<unknown>}
 */
export async function executeMachineOperation(request, overrides = {}) {
    if (!isRecord(request)) throw new Error('Machine request body must be a JSON object')
    if (typeof request.op !== 'string') {
        throw new Error('Machine request field "op" must be a string')
    }
    switch (request.op) {
        case 'checkout':
            return await createMachineRepository(request, overrides).checkout(
                requireString(request, 'branch'),
                { create: request.create === true }
            )
        case 'branch':
            return await createMachineRepository(request, overrides).listBranches()
        case 'commit':
            return await createMachineRepository(request, overrides).commit(
                requireString(request, 'message')
            )
        case 'log':
            return await createMachineRepository(request, overrides).log({
                branch: request.branch
            })
        case 'status':
            return await createMachineRepository(request, overrides).status()
        case 'diff':
            return await createMachineRepository(request, overrides).diff(
                request.from ?? 'HEAD',
                request.to ?? 'WORKTREE'
            )
        case 'restoreCommit':
            return await createMachineRepository(request, overrides).restoreCommit(
                requireString(request, 'id'),
                { force: request.force === true }
            )
        case 'merge':
            return await createMachineRepository(request, overrides).merge(
                requireString(request, 'source'),
                { message: request.message }
            )
    }
    const fylo = createMachineFylo(request, overrides)
    switch (request.op) {
        case 'executeSQL':
            return await fylo._sql(requireString(request, 'sql'))
        case 'createCollection': {
            const collection = requireString(request, 'collection')
            await fylo[collection].create()
            return { collection }
        }
        case 'dropCollection': {
            const collection = requireString(request, 'collection')
            await fylo[collection].drop()
            return { collection }
        }
        case 'inspectCollection':
            return await fylo[requireString(request, 'collection')].inspect()
        case 'rebuildCollection':
            return await fylo[requireString(request, 'collection')].rebuild()
        case 'getDoc':
            return await fylo[requireString(request, 'collection')]
                .get(requireString(request, 'id'))
                .once()
        case 'getLatest':
            return await fylo[requireString(request, 'collection')].latest(
                requireString(request, 'id'),
                request.onlyId === true
            )
        case 'findDocs':
            return await collectFindDocs(
                fylo,
                requireString(request, 'collection'),
                requireObject(request, 'query')
            )
        case 'findDeletedDocs':
            return await collectDeletedDocs(
                fylo,
                requireString(request, 'collection'),
                isRecord(request.query) ? request.query : {}
            )
        case 'joinDocs':
            return await fylo.joinDocs(
                /** @type {import('../query/types.js').StoreJoin<Record<string, any>, Record<string, any>>} */ (
                    requireObject(request, 'join')
                )
            )
        case 'putData':
            return await fylo[requireString(request, 'collection')].put(
                requireObject(request, 'data')
            )
        case 'batchPutData':
            return await fylo[requireString(request, 'collection')].batchPut(
                requireObjectArray(request, 'batch')
            )
        case 'patchDoc':
            return await fylo[requireString(request, 'collection')].patch(
                requireString(request, 'id'),
                requireObject(request, 'newDoc'),
                isRecord(request.oldDoc) ? request.oldDoc : {}
            )
        case 'patchDocs':
            return await fylo[requireString(request, 'collection')].patchMany(
                /** @type {import('../query/types.js').StoreUpdate<Record<string, any>>} */ (
                    requireObject(request, 'update')
                )
            )
        case 'delDoc':
            await fylo[requireString(request, 'collection')].delete(requireString(request, 'id'))
            return { deleted: true }
        case 'restoreDoc':
            await fylo[requireString(request, 'collection')].restore(requireString(request, 'id'))
            return { restored: true, id: requireString(request, 'id') }
        case 'delDocs':
            return await fylo[requireString(request, 'collection')].deleteMany(
                /** @type {import('../query/types.js').StoreDelete<Record<string, any>>} */ (
                    requireObject(request, 'delete')
                )
            )
        case 'importBulkData':
            return await fylo[requireString(request, 'collection')].import(
                new URL(requireString(request, 'url')),
                request.limitOrOptions
            )
        case 'schemaInspect':
            return await inspectSchema(requireString(request, 'collection'), request.schemaDir)
        case 'schemaCurrent': {
            const inspect = await inspectSchema(
                requireString(request, 'collection'),
                request.schemaDir
            )
            return {
                collection: inspect.collection,
                schemaDir: inspect.schemaDir,
                current: inspect.current
            }
        }
        case 'schemaHistory': {
            const inspect = await inspectSchema(
                requireString(request, 'collection'),
                request.schemaDir
            )
            return {
                collection: inspect.collection,
                schemaDir: inspect.schemaDir,
                versions: inspect.versions
            }
        }
        case 'schemaDoctor':
            return await doctorSchema(requireString(request, 'collection'), request.schemaDir)
        case 'schemaValidate':
            return await validateSchemaDocument(
                requireString(request, 'collection'),
                requireObject(request, 'document'),
                request.schemaDir
            )
        case 'schemaMaterialize':
            return await materializeSchemaDocument(
                requireString(request, 'collection'),
                requireObject(request, 'document'),
                request.schemaDir
            )
        default:
            throw new Error(`Unsupported machine operation: ${request.op}`)
    }
}

/**
 * @param {unknown} request
 * @param {MachineCliOverrides=} overrides
 * @returns {Promise<MachineSuccessResponse | MachineErrorResponse>}
 */
export async function runMachineRequest(request, overrides = {}) {
    const startedAt = Date.now()
    const safeRequest = isRecord(request) ? /** @type {Partial<MachineRequest>} */ (request) : {}
    try {
        const result = await executeMachineOperation(
            /** @type {MachineRequest} */ (request),
            overrides
        )
        return {
            protocolVersion: MACHINE_PROTOCOL_VERSION,
            ok: true,
            op: /** @type {MachineOperation} */ (safeRequest.op),
            requestId: typeof safeRequest.requestId === 'string' ? safeRequest.requestId : null,
            durationMs: Date.now() - startedAt,
            result
        }
    } catch (error) {
        const failure = /** @type {Error & { code?: string }} */ (error)
        return {
            protocolVersion: MACHINE_PROTOCOL_VERSION,
            ok: false,
            op:
                typeof safeRequest.op === 'string'
                    ? /** @type {MachineOperation} */ (safeRequest.op)
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

/**
 * @param {AsyncIterable<string | Buffer | Uint8Array>} stream
 * @returns {Promise<string>}
 */
export async function readTextStream(stream) {
    const chunks = []
    for await (const chunk of stream) {
        chunks.push(typeof chunk === 'string' ? Buffer.from(chunk) : Buffer.from(chunk))
    }
    return Buffer.concat(chunks).toString('utf8')
}

/**
 * @param {string | undefined} requestSource
 * @returns {Promise<string>}
 */
export async function loadMachineRequestText(requestSource) {
    if (!requestSource || requestSource === '-') {
        if (process.stdin.isTTY) {
            throw new Error('Machine request requires --request <json|@path> or stdin input')
        }
        return await readTextStream(process.stdin)
    }
    if (requestSource.startsWith('@')) {
        return await Bun.file(path.resolve(requestSource.slice(1))).text()
    }
    return requestSource
}

/**
 * @param {string | undefined} requestSource
 * @param {MachineCliOverrides=} overrides
 * @returns {Promise<MachineSuccessResponse | MachineErrorResponse>}
 */
export async function runMachineRequestSource(requestSource, overrides = {}) {
    try {
        const requestText = await loadMachineRequestText(requestSource)
        if (!requestText.trim()) {
            throw new Error('Machine request payload is empty')
        }
        return await runMachineRequest(JSON.parse(requestText), overrides)
    } catch (error) {
        const failure = /** @type {Error & { code?: string }} */ (error)
        return {
            protocolVersion: MACHINE_PROTOCOL_VERSION,
            ok: false,
            op: null,
            requestId: null,
            durationMs: 0,
            error: {
                name: failure.name || 'Error',
                message: failure.message || 'Unknown error',
                ...(typeof failure.code === 'string' ? { code: failure.code } : {})
            }
        }
    }
}
