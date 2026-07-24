import path from 'node:path'
import { AsyncLocalStorage } from 'node:async_hooks'
import { pathToFileURL } from 'node:url'
import Fylo from '../index.js'
import {
    createPosixGroupResolver,
    normalizeAccessInput,
    normalizeResolvedGroupIds
} from '../security/access.js'
import {
    doctorSchema,
    inspectSchema,
    materializeSchemaDocument,
    validateSchemaDocument
} from '../schema/admin.js'
import { VersionRepository } from '../versioning/repository.js'
import {
    BoundedNdjsonDecoder,
    MACHINE_PROTOCOL_VERSION,
    MachineFrameError,
    encodedJsonBytes,
    normalizeMachineFrameLimits,
    parseMachineFrame
} from './protocol.js'
import { acquireRootLease } from './root-lease.js'
import { runtimeIdentity } from './runtime-identity.js'
import { closeQueryCursorStates, collectQueryPage, queryCursorScope } from './query-page.js'

/**
 * @typedef {import('../replication/sync.js').FyloWormOptions} FyloWormOptions
 * @typedef {import('../replication/sync.js').FyloVersioningOptions} FyloVersioningOptions
 */

/**
 * @typedef {'handshake' | 'backupStatus' | 'backupReconcile' | 'executeSQL' | 'createCollection' | 'dropCollection' | 'inspectCollection' | 'rebuildCollection' | 'verifyCollection' | 'getDoc' | 'getLatest' | 'getMeta' | 'setMeta' | 'findDocs' | 'findDeletedDocs' | 'restoreDoc' | 'joinDocs' | 'putData' | 'batchPutData' | 'patchDoc' | 'patchDocs' | 'delDoc' | 'delDocs' | 'importBulkData' | 'checkout' | 'branch' | 'commit' | 'log' | 'status' | 'diff' | 'restoreCommit' | 'merge' | 'schemaInspect' | 'schemaCurrent' | 'schemaHistory' | 'schemaDoctor' | 'schemaValidate' | 'schemaMaterialize'} MachineOperation
 */

/**
 * @typedef {object} MachineRequest
 * @property {MachineOperation} op
 * @property {string=} requestId
 * @property {string=} root
 * @property {string=} schemaDir
 * @property {boolean | FyloWormOptions=} worm
 * @property {FyloVersioningOptions=} versioning
 * @property {string=} collection
 * @property {'document' | 'file'=} kind
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
 * @property {{ uid?: number, gid?: number, mode?: number, groups?: number[] }=} access trusted actor, optional per-request supplementary groups, and put/INSERT-only owner/group/mode
 * @property {Record<string, any>=} query
 * @property {{ limit?: number, cursor?: string }=} page bounded query continuation
 * @property {Record<string, any>=} join
 * @property {Record<string, any>=} document
 * @property {Record<string, any>=} data
 * @property {{ path?: string, url?: string, key?: string }=} file
 * @property {{ maxBytes?: number, key?: string, meta?: Record<string, any>, allowedProtocols?: string[], allowedHosts?: string[], allowPrivateNetwork?: boolean }=} fileOptions
 * @property {Record<string, any>=} meta developer metadata (putData initial record, or setMeta payload)
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
 * @property {FyloVersioningOptions=} versioning
 * @property {import('../replication/sync.js').FyloSyncHooks=} sync
 * @property {boolean=} allowFilePaths
 * @property {Map<string, any>=} cache Warm instances reused across requests (stdio loop)
 * @property {{ maxRequestBytes?: number, maxResponseBytes?: number }=} frameLimits
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
 * @property {{ name: string, message: string, code: string }} error
 */

/**
 * @param {unknown} value
 * @returns {value is Record<string, any>}
 */
function isRecord(value) {
    return typeof value === 'object' && value !== null && !Array.isArray(value)
}

class MachineOperationError extends Error {
    /** @param {string} code @param {string} message */
    constructor(code, message) {
        super(message)
        this.name = 'MachineOperationError'
        this.code = code
    }
}

/**
 * @param {MachineRequest} request
 * @param {keyof MachineRequest} field
 * @returns {string}
 */
function requireString(request, field) {
    const value = request[field]
    if (typeof value !== 'string' || value.trim().length === 0) {
        throw new MachineOperationError(
            'EBADREQUEST',
            `Machine request field "${String(field)}" must be a non-empty string`
        )
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
        throw new MachineOperationError(
            'EBADREQUEST',
            `Machine request field "${String(field)}" must be an object`
        )
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
        throw new MachineOperationError(
            'EBADREQUEST',
            `Machine request field "${String(field)}" must be an array of objects`
        )
    }
    return value
}

const machineAccessContext = new AsyncLocalStorage()
const resolvePosixGroups = createPosixGroupResolver()

/**
 * Group resolution for cached machine-mode engines. A trusted per-request
 * context takes precedence over the host account database and is isolated
 * across concurrent requests by AsyncLocalStorage.
 *
 * @param {number} uid
 * @returns {Promise<Set<number>>}
 */
async function resolveMachineGroups(uid) {
    const context = machineAccessContext.getStore()
    if (context) {
        if (context.uid !== uid) return new Set()
        return new Set(context.groups)
    }
    return await resolvePosixGroups(uid)
}

/**
 * Validate a machine access object and strip the machine-only `groups` field
 * before forwarding the native `.as(...)` context.
 *
 * @param {MachineRequest} request
 * @param {{ allowMode: boolean }} options
 * @returns {{ uid?: number, gid?: number, mode?: number } | undefined}
 */
function machineAccess(request, options) {
    if (request.access === undefined) return undefined
    const input = requireObject(request, 'access')
    const { groups, ...nativeAccess } = input
    try {
        if (groups !== undefined) {
            if (!Array.isArray(groups)) {
                throw new TypeError('Machine access.groups must be an array of numeric GIDs')
            }
            if (!Object.hasOwn(nativeAccess, 'uid')) {
                throw new TypeError('Machine access.groups requires access.uid')
            }
            normalizeResolvedGroupIds(groups)
        }
        return options.allowMode
            ? normalizeAccessInput(nativeAccess, { allowMode: true })
            : normalizeAccessInput(nativeAccess, { allowMode: false })
    } catch (error) {
        throw new MachineOperationError('EBADREQUEST', /** @type {Error} */ (error).message)
    }
}

/**
 * @param {MachineRequest} request
 * @returns {{ uid: number, groups: Set<number> } | undefined}
 */
function machineTrustedGroupContext(request) {
    if (!isRecord(request.access) || !Object.hasOwn(request.access, 'groups')) return undefined
    const access = machineAccess(request, { allowMode: true })
    return {
        uid: /** @type {number} */ (access?.uid),
        groups: normalizeResolvedGroupIds(request.access.groups)
    }
}

/**
 * @template T
 * @param {PromiseLike<T> & { as?: (access: any) => any }} operation
 * @param {{ uid?: number, gid?: number, mode?: number } | undefined} access
 * @returns {Promise<T>}
 */
async function runWithAccess(operation, access) {
    return await (access && operation.as ? operation.as(access) : operation)
}

/**
 * @param {MachineRequest} request
 * @param {MachineCliOverrides} overrides
 * @returns {URL | null}
 */
function machineFileInput(request, overrides) {
    if (request.file === undefined) return null
    if (!isRecord(request.file)) {
        throw new MachineOperationError(
            'EBADREQUEST',
            'Machine request field "file" must be an object'
        )
    }
    const filePath = request.file.path
    const fileUrl = request.file.url
    if ((filePath === undefined) === (fileUrl === undefined)) {
        throw new MachineOperationError(
            'EBADREQUEST',
            'Machine file input requires exactly one of "path" or "url"'
        )
    }
    if (filePath !== undefined) {
        if (typeof filePath !== 'string' || !path.isAbsolute(filePath)) {
            throw new MachineOperationError(
                'EBADREQUEST',
                'Machine file path must be an absolute path'
            )
        }
        if (overrides.allowFilePaths === false) {
            throw new MachineOperationError(
                'EBADREQUEST',
                'Local file paths are not allowed through this transport'
            )
        }
        return pathToFileURL(filePath)
    }
    if (typeof fileUrl !== 'string') {
        throw new MachineOperationError('EBADREQUEST', 'Machine file URL must be a string')
    }
    const parsed = new URL(fileUrl)
    if (parsed.protocol === 'file:' && overrides.allowFilePaths === false) {
        throw new MachineOperationError(
            'EBADREQUEST',
            'Local file paths are not allowed through this transport'
        )
    }
    return parsed
}

/**
 * @param {boolean | FyloWormOptions | undefined} worm
 * @returns {FyloWormOptions | undefined}
 */
function normalizeWormOptions(worm) {
    if (worm === undefined || worm === false) return undefined
    if (worm === true) return { mode: 'strict' }
    if (!isRecord(worm)) {
        throw new MachineOperationError(
            'EBADREQUEST',
            'Machine request field "worm" must be a boolean or object'
        )
    }
    const mode = worm.mode ?? 'strict'
    if (mode !== 'off' && mode !== 'strict') {
        throw new MachineOperationError(
            'EBADREQUEST',
            'Machine request field "worm.mode" must be "off" or "strict"'
        )
    }
    return { mode }
}

/**
 * @param {unknown} versioning
 * @returns {FyloVersioningOptions | undefined}
 */
function normalizeVersioningOptions(versioning) {
    if (versioning === undefined) return undefined
    if (!isRecord(versioning)) {
        throw new MachineOperationError(
            'EBADREQUEST',
            'Machine request field "versioning" must be an object'
        )
    }
    /** @type {FyloVersioningOptions} */
    const normalized = {}
    if (Object.hasOwn(versioning, 'resolve')) {
        if (typeof versioning.resolve !== 'boolean') {
            throw new MachineOperationError(
                'EBADREQUEST',
                'Machine request field "versioning.resolve" must be a boolean'
            )
        }
        normalized.resolve = versioning.resolve
    }
    if (Object.hasOwn(versioning, 'autoCommit')) {
        if (typeof versioning.autoCommit !== 'boolean') {
            throw new MachineOperationError(
                'EBADREQUEST',
                'Machine request field "versioning.autoCommit" must be a boolean'
            )
        }
        normalized.autoCommit = versioning.autoCommit
    }
    if (Object.hasOwn(versioning, 'repositoryRoot')) {
        if (
            typeof versioning.repositoryRoot !== 'string' ||
            versioning.repositoryRoot.length === 0
        ) {
            throw new MachineOperationError(
                'EBADREQUEST',
                'Machine request field "versioning.repositoryRoot" must be a non-empty string'
            )
        }
        normalized.repositoryRoot = versioning.repositoryRoot
    }
    return normalized
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
    const requestVersioning = normalizeVersioningOptions(request.versioning)
    const versioning =
        requestVersioning || overrides.versioning
            ? { ...(requestVersioning ?? {}), ...(overrides.versioning ?? {}) }
            : undefined
    const resolvedRoot = path.resolve(root ?? Fylo.defaultRoot())
    const build = () =>
        /** @type {import('../api/fylo.js').FyloCollections} */ (
            /** @type {unknown} */ (
                new Fylo(resolvedRoot, {
                    ...(worm ? { worm } : {}),
                    ...(versioning ? { versioning } : {}),
                    ...(overrides.sync ? { sync: overrides.sync } : {}),
                    access: { groupsForUid: resolveMachineGroups }
                })
            )
        )
    if (!overrides.cache) return build()
    const backupIdentity = overrides.sync?.s3
        ? {
              bucket: overrides.sync.s3.bucket,
              prefix: overrides.sync.s3.prefix,
              endpoint: overrides.sync.s3.endpoint,
              region: overrides.sync.s3.region
          }
        : null
    const key = `fylo:${resolvedRoot}:${JSON.stringify(worm ?? null)}:${JSON.stringify(versioning ?? null)}:${JSON.stringify(backupIdentity)}`
    let instance = overrides.cache.get(key)
    if (!instance) {
        instance = build()
        overrides.cache.set(key, instance)
    }
    return instance
}

/**
 * @param {MachineRequest} request
 * @param {MachineCliOverrides=} overrides
 * @returns {VersionRepository}
 */
function createMachineRepository(request, overrides = {}) {
    const resolvedRoot = path.resolve(overrides.root ?? request.root ?? Fylo.defaultRoot())
    if (!overrides.cache) return new VersionRepository(resolvedRoot)
    const key = `repo:${resolvedRoot}`
    let instance = overrides.cache.get(key)
    if (!instance) {
        instance = new VersionRepository(resolvedRoot)
        overrides.cache.set(key, instance)
    }
    return instance
}

/**
 * @param {import('../api/fylo.js').FyloCollections} fylo
 * @param {string} collection
 * @param {Record<string, any>} query
 * @param {{ uid: number } | undefined} access
 * @returns {Promise<Record<string, any> | string[]>}
 */
async function collectFindDocs(fylo, collection, query, access) {
    /** @type {Record<string, any> | string[]} */
    let docs = query.$onlyIds ? [] : {}
    const cursor = fylo[collection].find(query)
    if (access) cursor.as(access)
    for await (const value of cursor.collect()) {
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
 * @param {{ uid: number } | undefined} access
 * @returns {Promise<Record<string, any> | string[]>}
 */
async function collectDeletedDocs(fylo, collection, query, access) {
    /** @type {Record<string, any> | string[]} */
    let docs = query.$onlyIds ? [] : {}
    const cursor = fylo[collection].find.deleted(query)
    if (access) cursor.as(access)
    for await (const value of cursor.collect()) {
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
 * @param {MachineRequest} request
 * @param {MachineCliOverrides} overrides
 * @param {boolean} deleted
 * @param {{ uid: number } | undefined} access
 */
async function collectMachineQueryPage(fylo, request, overrides, deleted, access) {
    const collection = requireString(request, 'collection')
    const query = deleted
        ? isRecord(request.query)
            ? request.query
            : {}
        : requireObject(request, 'query')
    if (!isRecord(request.page)) {
        throw new MachineOperationError('EBADREQUEST', 'Machine query page must be an object')
    }
    const cursorToken = request.page.cursor
    if (cursorToken !== undefined && typeof cursorToken !== 'string') {
        throw new MachineOperationError('EBADREQUEST', 'Machine query page.cursor must be a string')
    }
    const cache = overrides.cache
    if (!cache) {
        throw new MachineFrameError(
            'EQUERYLOOPREQUIRED',
            'Machine query pagination requires fylo exec --loop'
        )
    }
    const cursorKey = 'machine:query-cursors'
    /** @type {Map<string, import('./query-page.js').QueryCursorState>} */
    let cursors = cache.get(cursorKey)
    if (!cursors) {
        cursors = new Map()
        cache.set(cursorKey, cursors)
    }
    let source
    if (!cursorToken) {
        const cursor = deleted ? fylo[collection].find.deleted(query) : fylo[collection].find(query)
        if (access) cursor.as(access)
        source = cursor.collect()
    }
    return await collectQueryPage(cursors, source, {
        onlyIds: query.$onlyIds === true,
        scope: queryCursorScope({
            op: request.op,
            collection,
            query,
            access: request.access
        }),
        cursor: cursorToken,
        limit: request.page.limit,
        maxResponseBytes: normalizeMachineFrameLimits(overrides.frameLimits).maxResponseBytes
    })
}

/**
 * @param {MachineRequest} request
 * @param {MachineCliOverrides=} overrides
 * @returns {Promise<unknown>}
 */
async function executeMachineOperationInContext(request, overrides = {}) {
    if (!isRecord(request))
        throw new MachineOperationError('EBADREQUEST', 'Machine request body must be a JSON object')
    if (typeof request.op !== 'string') {
        throw new MachineOperationError(
            'EBADREQUEST',
            'Machine request field "op" must be a string'
        )
    }
    switch (request.op) {
        case 'handshake':
            return runtimeIdentity(overrides.frameLimits, {
                backupConfigured: Boolean(overrides.sync?.s3)
            })
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
        case 'backupStatus':
            return (
                fylo.backupStatus() ?? {
                    configured: false,
                    state: 'disabled',
                    runs: 0
                }
            )
        case 'backupReconcile': {
            if (!fylo.backupStatus()) {
                throw new MachineOperationError(
                    'EBACKUPNOTCONFIGURED',
                    'Whole-root S3 backup is not configured'
                )
            }
            await fylo.reconcile()
            return fylo.backupStatus()
        }
        case 'executeSQL':
            return await fylo._sql(
                requireString(request, 'sql'),
                machineAccess(request, { allowMode: true })
            )
        case 'createCollection': {
            const collection = requireString(request, 'collection')
            const kind = request.kind ?? 'document'
            if (kind !== 'document' && kind !== 'file') {
                throw new MachineOperationError(
                    'EBADREQUEST',
                    'Machine request field "kind" must be "document" or "file"'
                )
            }
            await fylo[collection].create({ kind })
            return { collection, kind }
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
        case 'verifyCollection':
            return await fylo[requireString(request, 'collection')].verify()
        case 'getMeta': {
            const access = machineAccess(request, { allowMode: false })
            const operation = fylo[requireString(request, 'collection')].get(
                requireString(request, 'id')
            )
            if (access) operation.as(/** @type {{ uid: number }} */ (access))
            return await operation.metadata()
        }
        case 'setMeta': {
            const collection = requireString(request, 'collection')
            const id = requireString(request, 'id')
            const access = machineAccess(request, { allowMode: false })
            await runWithAccess(
                fylo[collection].put(id).metadata(requireObject(request, 'meta')),
                access
            )
            const operation = fylo[collection].get(id)
            if (access) operation.as(/** @type {{ uid: number }} */ (access))
            return await operation.metadata()
        }
        case 'getDoc': {
            const access = machineAccess(request, { allowMode: false })
            const operation = fylo[requireString(request, 'collection')].get(
                requireString(request, 'id')
            )
            if (access) operation.as(/** @type {{ uid: number }} */ (access))
            return await operation.once()
        }
        case 'getLatest': {
            const access = machineAccess(request, { allowMode: false })
            return await runWithAccess(
                fylo[requireString(request, 'collection')].latest(
                    requireString(request, 'id'),
                    request.onlyId === true
                ),
                access
            )
        }
        case 'findDocs': {
            const access = machineAccess(request, { allowMode: false })
            if (request.page !== undefined) {
                return await collectMachineQueryPage(
                    fylo,
                    request,
                    overrides,
                    false,
                    /** @type {{ uid: number } | undefined} */ (access)
                )
            }
            return await collectFindDocs(
                fylo,
                requireString(request, 'collection'),
                requireObject(request, 'query'),
                /** @type {{ uid: number } | undefined} */ (access)
            )
        }
        case 'findDeletedDocs': {
            const access = machineAccess(request, { allowMode: false })
            if (request.page !== undefined) {
                return await collectMachineQueryPage(
                    fylo,
                    request,
                    overrides,
                    true,
                    /** @type {{ uid: number } | undefined} */ (access)
                )
            }
            return await collectDeletedDocs(
                fylo,
                requireString(request, 'collection'),
                isRecord(request.query) ? request.query : {},
                /** @type {{ uid: number } | undefined} */ (access)
            )
        }
        case 'joinDocs': {
            const access = machineAccess(request, { allowMode: false })
            return await fylo.join(
                /** @type {import('../query/types.js').StoreJoin<Record<string, any>, Record<string, any>>} */ (
                    requireObject(request, 'join')
                ),
                access?.uid
            )
        }
        case 'putData': {
            const collection = requireString(request, 'collection')
            const file = machineFileInput(request, overrides)
            const access = machineAccess(request, { allowMode: true })
            if (file) {
                return await runWithAccess(
                    fylo[collection].put(file, {
                        ...request.fileOptions,
                        key: request.file?.key ?? request.fileOptions?.key,
                        meta: request.meta ?? request.fileOptions?.meta
                    }),
                    access
                )
            }
            return await runWithAccess(
                fylo[collection].put(
                    requireObject(request, 'data'),
                    request.meta ? { meta: request.meta } : undefined
                ),
                access
            )
        }
        case 'batchPutData': {
            const collection = requireString(request, 'collection')
            const access = machineAccess(request, { allowMode: true })
            const batch = requireObjectArray(request, 'batch')
            if (!access) return await fylo[collection].put.batch(batch)
            return await fylo.runCoalesced(() =>
                Promise.all(batch.map((data) => runWithAccess(fylo[collection].put(data), access)))
            )
        }
        case 'patchDoc': {
            const access = machineAccess(request, { allowMode: false })
            return await runWithAccess(
                fylo[requireString(request, 'collection')].patch(
                    requireString(request, 'id'),
                    requireObject(request, 'newDoc'),
                    isRecord(request.oldDoc) ? request.oldDoc : {}
                ),
                access
            )
        }
        case 'patchDocs': {
            const access = machineAccess(request, { allowMode: false })
            return await runWithAccess(
                fylo[requireString(request, 'collection')].patch.many(
                    /** @type {import('../query/types.js').StoreUpdate<Record<string, any>>} */ (
                        requireObject(request, 'update')
                    )
                ),
                access
            )
        }
        case 'delDoc': {
            const access = machineAccess(request, { allowMode: false })
            await runWithAccess(
                fylo[requireString(request, 'collection')].delete(requireString(request, 'id')),
                access
            )
            return { deleted: true }
        }
        case 'restoreDoc': {
            const id = requireString(request, 'id')
            const access = machineAccess(request, { allowMode: false })
            await runWithAccess(fylo[requireString(request, 'collection')].restore(id), access)
            return { restored: true, id: requireString(request, 'id') }
        }
        case 'delDocs': {
            const access = machineAccess(request, { allowMode: false })
            return await runWithAccess(
                fylo[requireString(request, 'collection')].delete.many(
                    /** @type {import('../query/types.js').StoreDelete<Record<string, any>>} */ (
                        requireObject(request, 'delete')
                    )
                ),
                access
            )
        }
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
            throw new MachineOperationError(
                'EUNSUPPORTEDOP',
                `Unsupported machine operation: ${request.op}`
            )
    }
}

/**
 * Execute one machine request with any trusted virtual-group assertion scoped
 * to this asynchronous operation. The context never persists on a cached
 * engine and cannot leak into a following request.
 *
 * @param {MachineRequest} request
 * @param {MachineCliOverrides=} overrides
 * @returns {Promise<unknown>}
 */
export async function executeMachineOperation(request, overrides = {}) {
    if (!isRecord(request))
        throw new MachineOperationError('EBADREQUEST', 'Machine request body must be a JSON object')
    const context = machineTrustedGroupContext(request)
    return await machineAccessContext.run(context, () =>
        executeMachineOperationInContext(request, overrides)
    )
}

/**
 * @param {unknown} error
 * @param {{ op?: MachineOperation | null, requestId?: string | null, durationMs?: number }=} context
 * @returns {MachineErrorResponse}
 */
function machineErrorResponse(error, context = {}) {
    const failure = /** @type {Error & { code?: string }} */ (error)
    return {
        protocolVersion: MACHINE_PROTOCOL_VERSION,
        ok: false,
        op: context.op ?? null,
        requestId: context.requestId ?? null,
        durationMs: context.durationMs ?? 0,
        error: {
            name: failure.name || 'Error',
            message: failure.message || 'Unknown error',
            code:
                typeof failure.code === 'string' && failure.code.length > 0
                    ? failure.code
                    : 'EUNKNOWN'
        }
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
        return machineErrorResponse(error, {
            op:
                typeof safeRequest.op === 'string'
                    ? /** @type {MachineOperation} */ (safeRequest.op)
                    : null,
            requestId: typeof safeRequest.requestId === 'string' ? safeRequest.requestId : null,
            durationMs: Date.now() - startedAt
        })
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
            throw new MachineOperationError(
                'EBADREQUEST',
                'Machine request requires --request <json|@path> or stdin input'
            )
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
            throw new MachineOperationError('EBADREQUEST', 'Machine request payload is empty')
        }
        return await runMachineRequest(JSON.parse(requestText), overrides)
    } catch (error) {
        return machineErrorResponse(error)
    }
}

/**
 * @param {MachineSuccessResponse | MachineErrorResponse} response
 * @param {number} maximum
 */
function encodeMachineResponse(response, maximum) {
    let bytes = encodedJsonBytes(response)
    if (bytes.byteLength > maximum) {
        bytes = encodedJsonBytes(
            machineErrorResponse(
                new MachineFrameError(
                    'EFRAME_RESPONSE_TOO_LARGE',
                    `Machine response exceeds ${maximum} bytes; narrow or paginate the operation`
                ),
                { op: response.op, requestId: response.requestId }
            )
        )
    }
    if (bytes.byteLength > maximum) {
        throw new RangeError(`Machine response limit ${maximum} cannot hold an error envelope`)
    }
    return `${Buffer.from(bytes).toString('utf8')}\n`
}

/**
 * Persistent bounded NDJSON loop. Request and response limits exclude the LF
 * delimiter. Malformed frames resume only at a known LF boundary; truncated
 * EOF emits one error and terminates naturally.
 *
 * @param {object} [options]
 * @param {AsyncIterable<Uint8Array | string>} [options.input] Defaults to process.stdin
 * @param {(line: string) => void | Promise<void>} [options.write] Defaults to stdout
 * @param {MachineCliOverrides} [options.overrides]
 * @param {{ maxRequestBytes?: number, maxResponseBytes?: number }} [options.frameLimits]
 * @param {boolean} [options.exclusiveRoot]
 * @returns {Promise<boolean>} false when exclusive ownership cannot be acquired or is lost
 */
export async function serveStdioLoop(options = {}) {
    const input = options.input ?? process.stdin
    const write =
        options.write ??
        ((line) => new Promise((resolve) => process.stdout.write(line, () => resolve())))
    const frameLimits = normalizeMachineFrameLimits(options.frameLimits)
    const overrides = {
        ...(options.overrides ?? {}),
        cache: options.overrides?.cache ?? new Map(),
        frameLimits
    }
    const decoder = new BoundedNdjsonDecoder(frameLimits.maxRequestBytes)
    const emit = async (/** @type {MachineSuccessResponse | MachineErrorResponse} */ response) => {
        await write(encodeMachineResponse(response, frameLimits.maxResponseBytes))
    }
    /** @type {import('./root-lease.js').FyloRootLease | undefined} */
    let lease
    if (options.exclusiveRoot) {
        const requestedRoot = path.resolve(overrides.root ?? Fylo.defaultRoot())
        const repositoryRoot = VersionRepository.resolveRepositoryRoot(requestedRoot)
        try {
            lease = await acquireRootLease(repositoryRoot)
        } catch (error) {
            await emit(machineErrorResponse(error))
            return false
        }
    }

    const handle = async (
        /** @type {{ frame?: Uint8Array, error?: MachineFrameError }} */ event
    ) => {
        if (event.error) {
            await emit(machineErrorResponse(event.error))
            return true
        }
        let request
        try {
            request = parseMachineFrame(/** @type {Uint8Array} */ (event.frame))
        } catch (error) {
            await emit(machineErrorResponse(error))
            return true
        }
        if (request === null) return true
        if (lease) {
            try {
                await lease.assertOwned()
            } catch (error) {
                await emit(machineErrorResponse(error))
                return false
            }
        }
        await emit(await runMachineRequest(request, overrides))
        return true
    }

    try {
        for await (const chunk of input) {
            for (const event of decoder.push(chunk)) {
                if (!(await handle(event))) return false
            }
        }
        for (const event of decoder.finish()) {
            if (!(await handle(event))) return false
        }
        return true
    } finally {
        const cache = overrides.cache
        if (cache) {
            const cursors = cache.get('machine:query-cursors')
            if (cursors instanceof Map) closeQueryCursorStates(cursors)
            const closers = [...cache.values()]
                .filter((value) => value && typeof value.close === 'function')
                .map((value) => Promise.resolve(value.close()))
            await Promise.allSettled(closers)
            cache.clear()
        }
        await lease?.release()
    }
}
