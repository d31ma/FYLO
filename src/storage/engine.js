import path from 'node:path'
import { CollectionNotFoundError, validateCollectionName } from '../core/collection.js'
import { assertPathInside, validateDocId } from '../core/doc-id.js'
import { Cipher } from '../security/cipher.js'
import { FyloSyncError, resolveSyncMode } from '../replication/sync.js'
import { emitFyloEvent } from '../observability/events.js'
import { FilesystemEventBus, FilesystemLockManager, FilesystemStorage } from './primitives.js'
import { FilesystemDocuments } from './documents.js'
import { FilesystemQueryEngine } from './query.js'
import { materializeDoc } from '../schema/migrate.js'
import { BunS3ClientIndexStore, LocalFsPrefixIndexStore } from './prefix-index.js'
import { parseStoredValue, stringifyStoredValue } from './value-codec.js'

/**
 * @typedef {import('../types/vendor.js').TTID} TTID
 * @typedef {import('../replication/sync.js').FyloSyncHooks<Record<string, any>>} FyloSyncHooks
 * @typedef {import('../replication/sync.js').FyloSyncMode} FyloSyncMode
 * @typedef {import('../replication/sync.js').FyloWriteSyncEvent<Record<string, any>>} FyloWriteSyncEvent
 * @typedef {import('../replication/sync.js').FyloDeleteSyncEvent} FyloDeleteSyncEvent
 * @typedef {import('../replication/sync.js').FyloWormOptions} FyloWormOptions
 * @typedef {import('../observability/events.js').FyloEventHandler} FyloEventHandler
 * @typedef {import('./types.js').StorageEngine} StorageEngine
 * @typedef {import('./types.js').LockManager} LockManager
 * @typedef {import('./types.js').EventBus<Record<string, any>>} EventBus
 * @typedef {import('./types.js').CollectionInspectResult} CollectionInspectResult
 * @typedef {import('./types.js').CollectionRebuildResult} CollectionRebuildResult
 * @typedef {import('./types.js').FilesystemEvent<Record<string, any>>} FilesystemEvent
 * @typedef {import('./types.js').StoredDoc<Record<string, any>>} StoredDoc
 * @typedef {import('./types.js').PrefixIndexStore} PrefixIndexStore
 * @typedef {import('../queue/local.js').LocalQueue} LocalQueue
 * @typedef {import('../cache/query.js').QueryCache} QueryCache
 * @typedef {import('../query/types.js').StoreJoin<Record<string, any>, Record<string, any>>} StoreJoin
 * @typedef {import('../query/types.js').StoreQuery<Record<string, any>>} StoreQuery
 */

/**
 * Low-level filesystem storage engine for collections, documents, indexes,
 * events, locks, and strict read-only WORM documents.
 */
export class FilesystemEngine {
    /** @type {string} */
    root
    /** @type {'filesystem'} */
    kind = 'filesystem'
    /** @type {Map<string, Promise<void>>} */
    writeLanes = new Map()
    /** @type {Map<string, Promise<TTID[]>>} */
    cacheMissLanes = new Map()
    /** @type {StorageEngine} */
    storage
    /** @type {LockManager} */
    locks
    /** @type {EventBus} */
    events
    /** @type {LocalQueue | undefined} */
    queue
    /** @type {QueryCache | undefined} */
    queryCache
    /** @type {FilesystemDocuments} */
    documents
    /** @type {FilesystemQueryEngine} */
    queryEngine
    /** @type {PrefixIndexStore} */
    index
    /** @type {FyloSyncHooks | undefined} */
    sync
    /** @type {FyloSyncMode} */
    syncMode
    /** @type {Required<FyloWormOptions>} */
    worm
    /** @type {FyloEventHandler | undefined} */
    onEvent
    /**
     * Creates the filesystem-backed FYLO engine and its persistence collaborators.
     * @param {string} [root]
     * @param {{ sync?: FyloSyncHooks, syncMode?: FyloSyncMode, worm?: FyloWormOptions, onEvent?: FyloEventHandler, index?: import('./types.js').FyloIndexOptions, queue?: LocalQueue, queryCache?: QueryCache }} [options]
     */
    constructor(
        root = process.env.FYLO_ROOT || path.join(process.cwd(), '.fylo-data'),
        options = {}
    ) {
        this.root = root
        this.sync = options.sync
        this.syncMode = resolveSyncMode(options.syncMode)
        this.worm = {
            mode: options.worm?.mode ?? 'off'
        }
        this.onEvent = options.onEvent
        this.storage = new FilesystemStorage()
        this.locks = new FilesystemLockManager(this.root, this.storage)
        this.events = new FilesystemEventBus(this.root, this.storage)
        this.queue = options.queue
        this.queryCache = options.queryCache
        this.index =
            options.index?.backend === 's3-client'
                ? new BunS3ClientIndexStore(options.index.s3)
                : new LocalFsPrefixIndexStore(this.collectionRoot.bind(this))
        this.documents = new FilesystemDocuments(
            this.storage,
            this.docsRoot.bind(this),
            this.docPath.bind(this),
            this.deletedRoot.bind(this),
            this.deletedPath.bind(this),
            this.ensureCollection.bind(this),
            this.encodeEncrypted.bind(this),
            this.decodeEncrypted.bind(this)
        )
        this.queryEngine = new FilesystemQueryEngine({
            index: this.index
        })
    }
    /**
     * @param {string} collection
     * @param {FilesystemEvent} event
     * @returns {Promise<void>}
     */
    async publishDocumentEvent(collection, event) {
        await this.events.publish(collection, event)
        await this.queue?.publishCollectionEvent(collection, event)
    }
    /** @param {string} collection @returns {string} */
    collectionRoot(collection) {
        validateCollectionName(collection)
        return path.join(this.root, '.collections', collection)
    }
    /** @param {string} collection @returns {string} */
    docsRoot(collection) {
        return path.join(this.collectionRoot(collection), 'docs')
    }
    /** @param {string} collection @returns {string} */
    deletedRoot(collection) {
        return path.join(this.collectionRoot(collection), '.deleted')
    }
    /** @param {string} collection @returns {string} */
    metaRoot(collection) {
        return this.collectionRoot(collection)
    }
    /** @param {string} collection @param {TTID} docId @returns {string} */
    docPath(collection, docId) {
        validateDocId(docId)
        const docsRoot = this.docsRoot(collection)
        const target = path.join(docsRoot, docId.slice(0, 2), `${docId}.json`)
        assertPathInside(docsRoot, target)
        return target
    }
    /** @param {string} collection @param {TTID} docId @returns {string} */
    deletedPath(collection, docId) {
        validateDocId(docId)
        const deletedRoot = this.deletedRoot(collection)
        const target = path.join(deletedRoot, docId.slice(0, 2), `${docId}.json`)
        assertPathInside(deletedRoot, target)
        return target
    }
    /**
     * Runs a configured sync hook according to the collection's sync mode.
     * @param {string} collection
     * @param {TTID} docId
     * @param {'insert' | 'delete' | string} operation
     * @param {string} targetPath
     * @param {() => Promise<void>} task
     * @returns {Promise<void>}
     */
    async runSyncTask(collection, docId, operation, targetPath, task) {
        if (!this.sync?.onWrite && !this.sync?.onDelete) return
        if (this.syncMode === 'fire-and-forget') {
            void task().catch((cause) => {
                const error = new FyloSyncError({
                    collection,
                    docId,
                    operation,
                    path: targetPath,
                    cause
                })
                console.error(error)
                emitFyloEvent(this.onEvent, {
                    type: 'sync.failed',
                    collection,
                    docId: String(docId),
                    operation,
                    path: targetPath,
                    detail: cause instanceof Error ? cause.message : String(cause)
                })
            })
            return
        }
        try {
            await task()
        } catch (cause) {
            throw new FyloSyncError({
                collection,
                docId,
                operation,
                path: targetPath,
                cause
            })
        }
    }
    /** @param {FyloWriteSyncEvent} event @returns {Promise<void>} */
    async syncWrite(event) {
        if (!this.sync?.onWrite) return
        await this.sync.onWrite(event)
    }
    /** @param {FyloDeleteSyncEvent} event @returns {Promise<void>} */
    async syncDelete(event) {
        if (!this.sync?.onDelete) return
        await this.sync.onDelete(event)
    }
    /** @param {string} collection @returns {Promise<void>} */
    async ensureCollection(collection) {
        await this.storage.mkdir(this.collectionRoot(collection))
        await this.assertNoLegacyWormArtifacts(collection)
        await this.storage.mkdir(this.metaRoot(collection))
        await this.storage.mkdir(this.docsRoot(collection))
        await this.storage.mkdir(this.deletedRoot(collection))
        await this.index.ensureCollection(collection)
    }
    /** @param {string} collection @returns {Promise<void>} */
    async requireCollection(collection) {
        if (!(await this.hasCollection(collection))) throw new CollectionNotFoundError(collection)
    }
    /**
     * Refuses to reinterpret append-only WORM files as independent live docs
     * after the strict WORM storage format change.
     * @param {string} collection
     * @returns {Promise<void>}
     */
    async assertNoLegacyWormArtifacts(collection) {
        const legacyPaths = ['heads', 'versions']
        for (const legacyPath of legacyPaths) {
            const files = await this.storage.list(
                path.join(this.collectionRoot(collection), legacyPath)
            )
            if (files.length > 0) {
                throw new Error(
                    `Collection '${collection}' contains unsupported legacy WORM ${legacyPath} metadata; migrate or remove legacy versions before opening it`
                )
            }
        }
    }
    /** @returns {boolean} */
    wormEnabled() {
        return this.worm.mode === 'strict'
    }
    /** @param {string} collection @returns {Promise<void>} */
    async invalidateQueryCache(collection) {
        if (!this.queryCache) return
        try {
            await this.queryCache.bumpCollection(collection)
        } catch (err) {
            if (this.queryCache.required || this.queryCache.method === 'write-through') throw err
        }
    }
    /** @param {string} collection @returns {Promise<number | null>} */
    async queryCacheVersion(collection) {
        if (!this.queryCache) return null
        try {
            return await this.queryCache.version(collection)
        } catch (err) {
            if (this.queryCache.required) throw err
            return null
        }
    }
    /**
     * @param {'active' | 'deleted'} kind
     * @param {string} collection
     * @param {StoreQuery | undefined} query
     * @param {number | null} version
     * @returns {Promise<TTID[] | null>}
     */
    async cachedQueryIds(kind, collection, query, version) {
        if (!this.queryCache || version === null) return null
        try {
            return await this.queryCache.getIds(kind, collection, version, query)
        } catch (err) {
            if (this.queryCache.required) throw err
            return null
        }
    }
    /**
     * @param {'active' | 'deleted'} kind
     * @param {string} collection
     * @param {StoreQuery | undefined} query
     * @param {number | null} version
     * @param {TTID[]} ids
     * @returns {Promise<void>}
     */
    async cacheQueryIds(kind, collection, query, version, ids) {
        if (!this.queryCache || version === null) return
        try {
            if ((await this.queryCache.version(collection)) !== version) return
            await this.queryCache.setIds(kind, collection, version, query, ids)
        } catch (err) {
            if (this.queryCache.required) throw err
        }
    }
    /**
     * @param {'active' | 'deleted'} kind
     * @param {string} collection
     * @param {StoreQuery | undefined} query
     * @param {number | null} version
     * @param {() => Promise<TTID[]>} loadIds
     * @returns {Promise<TTID[]>}
     */
    async loadCacheMissIds(kind, collection, query, version, loadIds) {
        if (!this.queryCache?.stampedeProtection || version === null) return await loadIds()
        const key = this.queryCache.key(kind, collection, version, query)
        const existing = this.cacheMissLanes.get(key)
        if (existing) return await existing
        const lane = loadIds().finally(() => {
            if (this.cacheMissLanes.get(key) === lane) this.cacheMissLanes.delete(key)
        })
        this.cacheMissLanes.set(key, lane)
        return await lane
    }
    /** @param {string} collection @returns {Promise<void>} */
    async resetIndex(collection) {
        await this.index.resetCollection(collection)
        await this.invalidateQueryCache(collection)
    }
    /** @param {string} collection @returns {Promise<TTID[]>} */
    async listQueryableDocIds(collection) {
        await this.requireCollection(collection)
        await this.assertNoLegacyWormArtifacts(collection)
        return await this.documents.listDocIds(collection)
    }
    /**
     * Overwrites a mutable document while preserving its identity.
     * @param {string} collection
     * @param {TTID} docId
     * @param {Record<string, any>} nextDoc
     * @param {Record<string, any>} oldDoc
     * @returns {Promise<TTID>}
     */
    async updateDocument(collection, docId, nextDoc, oldDoc) {
        validateDocId(docId)
        if (this.wormEnabled()) throw new Error('Update is not allowed in WORM mode')
        await this.assertNoLegacyWormArtifacts(collection)
        return await this.withCollectionWriteLock(collection, async () => {
            const owner = Bun.randomUUIDv7()
            if (!(await this.locks.acquire(collection, docId, owner)))
                throw new Error(`Unable to acquire filesystem lock for ${docId}`)
            const targetPath = this.docPath(collection, docId)
            try {
                const existing =
                    oldDoc ?? (await this.documents.readStoredDoc(collection, docId))?.data
                if (!existing) return docId
                await this.removeIndexes(collection, docId, existing)
                await this.documents.writeStoredDoc(collection, docId, nextDoc)
                await this.rebuildIndexes(collection, docId, nextDoc)
                await this.publishDocumentEvent(collection, {
                    ts: Date.now(),
                    action: 'insert',
                    id: docId,
                    doc: await this.encodeEncrypted(collection, nextDoc)
                })
                await this.runSyncTask(collection, docId, 'patch', targetPath, async () => {
                    await this.syncWrite({
                        operation: 'patch',
                        collection,
                        docId,
                        path: targetPath,
                        data: nextDoc
                    })
                })
                await this.invalidateQueryCache(collection)
                return docId
            } finally {
                await this.locks.release(collection, docId, owner)
            }
        })
    }
    /** @template T @param {string} collection @param {() => Promise<T>} action @returns {Promise<T>} */
    async withCollectionWriteLock(collection, action) {
        const previous = this.writeLanes.get(collection) ?? Promise.resolve()
        /** @type {() => void} */
        let release = () => {}
        /** @type {Promise<void>} */
        const current = new Promise((resolve) => {
            release = () => resolve()
        })
        const lane = previous.then(() => current)
        this.writeLanes.set(collection, lane)
        await previous
        const owner = Bun.randomUUIDv7()
        try {
            await this.storage.mkdir(this.metaRoot(collection))
            await this.locks.acquireCollectionWrite(collection, owner, {
                onTakeover: (info) => {
                    emitFyloEvent(this.onEvent, {
                        type: 'lock.takeover',
                        lockPath: info.lockPath,
                        newOwner: info.newOwner,
                        previousOwner: info.previousOwner
                    })
                }
            })
            return await action()
        } finally {
            try {
                await this.locks.releaseCollectionWrite(collection, owner)
            } finally {
                release()
                if (this.writeLanes.get(collection) === lane) this.writeLanes.delete(collection)
            }
        }
    }
    /** @param {string} collection @returns {Promise<void>} */
    async createCollection(collection) {
        await this.ensureCollection(collection)
        await this.invalidateQueryCache(collection)
    }
    /** @param {string} collection @returns {Promise<void>} */
    async dropCollection(collection) {
        await this.requireCollection(collection)
        if (this.wormEnabled() && (await this.documents.listDocIds(collection)).length > 0) {
            throw new Error('Drop is not allowed for a non-empty WORM collection')
        }
        await this.storage.rmdir(this.collectionRoot(collection))
        await this.invalidateQueryCache(collection)
    }
    /** @param {string} collection @returns {Promise<boolean>} */
    async hasCollection(collection) {
        return await this.storage.exists(this.collectionRoot(collection))
    }
    /** @param {string} collection @returns {Promise<CollectionInspectResult>} */
    async inspectCollection(collection) {
        const exists = await this.hasCollection(collection)
        if (!exists) {
            return {
                collection,
                exists: false,
                worm: false,
                docsStored: 0,
                deletedDocs: 0,
                indexedDocs: 0
            }
        }
        const [docIds, deletedDocIds, indexedDocs] = await Promise.all([
            this.documents.listDocIds(collection),
            this.documents.listDeletedDocIds(collection),
            this.index.countDocuments(collection)
        ])
        return {
            collection,
            exists: true,
            worm: this.wormEnabled(),
            docsStored: docIds.length,
            deletedDocs: deletedDocIds.length,
            indexedDocs
        }
    }
    /** @param {string} collection @param {any} value @param {string} [parentField] @returns {Promise<any>} */
    async encodeEncrypted(collection, value, parentField) {
        if (Array.isArray(value)) {
            const encodedItems = await Promise.all(
                value.map(async (item) => {
                    if (item && typeof item === 'object')
                        return await this.encodeEncrypted(collection, item)
                    if (
                        parentField &&
                        Cipher.isConfigured() &&
                        Cipher.isEncryptedField(collection, parentField)
                    ) {
                        return await Cipher.encrypt(stringifyStoredValue(item))
                    }
                    return item
                })
            )
            return encodedItems
        }
        if (value && typeof value === 'object') {
            /** @type {Record<string, any>} */
            const copy = {}
            for (const field in value) {
                const nextField = parentField ? `${parentField}/${field}` : field
                const fieldValue = value[field]
                if (fieldValue && typeof fieldValue === 'object')
                    copy[field] = await this.encodeEncrypted(collection, fieldValue, nextField)
                else if (Cipher.isConfigured() && Cipher.isEncryptedField(collection, nextField)) {
                    copy[field] = await Cipher.encrypt(stringifyStoredValue(fieldValue))
                } else copy[field] = fieldValue
            }
            return copy
        }
        return value
    }
    /** @param {string} collection @param {any} value @param {string} [parentField] @returns {Promise<any>} */
    async decodeEncrypted(collection, value, parentField) {
        if (Array.isArray(value)) {
            const decodedItems = await Promise.all(
                value.map(async (item) => {
                    if (item && typeof item === 'object')
                        return await this.decodeEncrypted(collection, item)
                    if (
                        parentField &&
                        Cipher.isConfigured() &&
                        Cipher.isEncryptedField(collection, parentField) &&
                        typeof item === 'string'
                    ) {
                        return parseStoredValue((await Cipher.decrypt(item)).replaceAll('%2F', '/'))
                    }
                    return item
                })
            )
            return decodedItems
        }
        if (value && typeof value === 'object') {
            /** @type {Record<string, any>} */
            const copy = {}
            for (const field in value) {
                const nextField = parentField ? `${parentField}/${field}` : field
                const fieldValue = value[field]
                if (fieldValue && typeof fieldValue === 'object')
                    copy[field] = await this.decodeEncrypted(collection, fieldValue, nextField)
                else if (
                    Cipher.isConfigured() &&
                    Cipher.isEncryptedField(collection, nextField) &&
                    typeof fieldValue === 'string'
                ) {
                    copy[field] = parseStoredValue(
                        (await Cipher.decrypt(fieldValue)).replaceAll('%2F', '/')
                    )
                } else copy[field] = fieldValue
            }
            return copy
        }
        return value
    }
    /** @param {string} collection @param {StoreQuery | undefined} [query] @returns {Promise<Array<Record<string, Record<string, any>>>>} */
    async docResults(collection, query) {
        await this.requireCollection(collection)
        const cacheVersion = await this.queryCacheVersion(collection)
        const cachedIds = await this.cachedQueryIds('active', collection, query, cacheVersion)
        const ids =
            cachedIds ??
            (await this.loadCacheMissIds('active', collection, query, cacheVersion, async () => {
                const candidateIds = await this.queryEngine.candidateDocIdsForQuery(
                    collection,
                    query
                )
                return candidateIds
                    ? Array.from(candidateIds)
                    : await this.listQueryableDocIds(collection)
            }))
        const limit = query?.$limit
        /** @type {Array<Record<string, Record<string, any>>>} */
        const results = []
        /** @type {TTID[]} */
        const resultIds = []
        for (const id of ids) {
            const stored = await this.documents.readStoredDoc(collection, id)
            if (!stored) continue
            // Upgrade pre-match so queries against fields added in newer schema
            // versions can still hit older docs in storage. Note: indexes are
            // built from on-disk shape, so index-eligible queries on new-only
            // fields still require a rebuild after a schema bump.
            const data = /** @type {Record<string, any>} */ (
                await materializeDoc(collection, stored.data)
            )
            if (!this.queryEngine.matchesQuery(id, data, query, stored)) continue
            results.push({ [id]: data })
            resultIds.push(id)
            if (limit && results.length >= limit) break
        }
        if (!cachedIds)
            await this.cacheQueryIds('active', collection, query, cacheVersion, resultIds)
        return results
    }
    /** @param {string} collection @param {StoreQuery | undefined} [query] @returns {Promise<Array<Record<string, Record<string, any>>>>} */
    async deletedDocResults(collection, query) {
        await this.requireCollection(collection)
        const cacheVersion = await this.queryCacheVersion(collection)
        const cachedIds = await this.cachedQueryIds('deleted', collection, query, cacheVersion)
        const ids =
            cachedIds ??
            (await this.loadCacheMissIds('deleted', collection, query, cacheVersion, async () => {
                return await this.documents.listDeletedDocIds(collection)
            }))
        const limit = query?.$limit
        /** @type {Array<Record<string, Record<string, any>>>} */
        const results = []
        /** @type {TTID[]} */
        const resultIds = []
        for (const id of ids) {
            const stored = await this.documents.readDeletedDoc(collection, id)
            if (!stored) continue
            const data = /** @type {Record<string, any>} */ (
                await materializeDoc(collection, stored.data)
            )
            if (!this.queryEngine.matchesDeletedQuery(id, data, query, stored)) continue
            results.push({ [id]: data })
            resultIds.push(id)
            if (limit && results.length >= limit) break
        }
        if (!cachedIds)
            await this.cacheQueryIds('deleted', collection, query, cacheVersion, resultIds)
        return results
    }
    /** @param {string} collection @param {TTID} docId @param {Record<string, any>} doc @returns {Promise<void>} */
    async rebuildIndexes(collection, docId, doc) {
        await this.index.putDocument(collection, docId, doc)
    }
    /** @param {string} collection @param {TTID} docId @param {Record<string, any>} doc @returns {Promise<void>} */
    async removeIndexes(collection, docId, doc) {
        await this.index.removeDocument(collection, docId, doc)
    }
    /** @param {string} collection @returns {Promise<CollectionRebuildResult>} */
    async rebuildCollection(collection) {
        await this.requireCollection(collection)
        return await this.withCollectionWriteLock(collection, async () => {
            await this.ensureCollection(collection)
            const docIds = await this.documents.listDocIds(collection)
            let indexedDocs = 0
            await this.resetIndex(collection)
            for (const docId of docIds) {
                const stored = await this.documents.readStoredDoc(collection, docId)
                if (!stored) continue
                await this.index.putDocument(collection, docId, stored.data)
                indexedDocs++
            }
            emitFyloEvent(this.onEvent, {
                type: 'index.rebuilt',
                collection,
                docsScanned: docIds.length,
                indexedDocs,
                worm: this.wormEnabled()
            })
            return {
                collection,
                worm: this.wormEnabled(),
                docsScanned: docIds.length,
                indexedDocs
            }
        })
    }
    /** @param {string} collection @param {TTID} docId @param {Record<string, any>} doc @returns {Promise<void>} */
    async putDocument(collection, docId, doc) {
        validateDocId(docId)
        await this.requireCollection(collection)
        await this.assertNoLegacyWormArtifacts(collection)
        await this.withCollectionWriteLock(collection, async () => {
            const owner = Bun.randomUUIDv7()
            if (!(await this.locks.acquire(collection, docId, owner)))
                throw new Error(`Unable to acquire filesystem lock for ${docId}`)
            const targetPath = this.docPath(collection, docId)
            try {
                const existing = await this.documents.readStoredDoc(collection, docId)
                if (await this.documents.readDeletedDoc(collection, docId)) {
                    throw new Error(`Document is soft-deleted; restore it before writing: ${docId}`)
                }
                if (existing && this.wormEnabled())
                    throw new Error('Update is not allowed in WORM mode')
                if (existing) await this.removeIndexes(collection, docId, existing.data)
                await this.documents.writeStoredDoc(collection, docId, doc)
                await this.rebuildIndexes(collection, docId, doc)
                if (this.wormEnabled())
                    await this.documents.makeStoredDocReadOnly(collection, docId)
                await this.publishDocumentEvent(collection, {
                    ts: Date.now(),
                    action: 'insert',
                    id: docId,
                    doc: await this.encodeEncrypted(collection, doc)
                })
                await this.runSyncTask(collection, docId, 'put', targetPath, async () => {
                    await this.syncWrite({
                        operation: 'put',
                        collection,
                        docId,
                        path: targetPath,
                        data: doc
                    })
                })
                await this.invalidateQueryCache(collection)
            } finally {
                await this.locks.release(collection, docId, owner)
            }
        })
    }
    /** @param {string} collection @param {TTID} oldId @param {TTID} newId @param {Record<string, any>} patch @param {Record<string, any>} oldDoc @returns {Promise<TTID>} */
    async patchDocument(collection, oldId, newId, patch, oldDoc) {
        if (this.wormEnabled()) throw new Error('Update is not allowed in WORM mode')
        await this.requireCollection(collection)
        const existing = oldDoc ?? (await this.documents.readStoredDoc(collection, oldId))?.data
        if (!existing) return oldId
        const nextDoc = { ...existing, ...patch }
        return await this.updateDocument(collection, oldId, nextDoc, existing)
    }
    /** @param {string} collection @param {TTID} oldId @param {TTID} newId @param {Record<string, any>} doc @param {Record<string, any>} [oldDoc] @returns {Promise<TTID>} */
    async replaceDocumentVersion(collection, oldId, newId, doc, oldDoc) {
        if (this.wormEnabled()) throw new Error('Update is not allowed in WORM mode')
        await this.requireCollection(collection)
        if (oldDoc) return await this.updateDocument(collection, oldId, doc, oldDoc)
        const stored = await this.documents.readStoredDoc(collection, oldId)
        if (!stored && (await this.documents.readDeletedDoc(collection, oldId))) {
            throw new Error(`Document is soft-deleted; restore it before writing: ${oldId}`)
        }
        if (!stored) return oldId
        return await this.updateDocument(collection, oldId, doc, stored.data)
    }
    /** @param {string} collection @param {TTID} docId @returns {Promise<void>} */
    async deleteDocument(collection, docId) {
        validateDocId(docId)
        if (this.wormEnabled()) throw new Error('Delete is not allowed in WORM mode')
        await this.requireCollection(collection)
        await this.assertNoLegacyWormArtifacts(collection)
        await this.withCollectionWriteLock(collection, async () => {
            const owner = Bun.randomUUIDv7()
            if (!(await this.locks.acquire(collection, docId, owner)))
                throw new Error(`Unable to acquire filesystem lock for ${docId}`)
            try {
                const existing = await this.documents.readStoredDoc(collection, docId)
                if (!existing) return
                await this.removeIndexes(collection, docId, existing.data)
                const deletedAt = Date.now()
                const deletedPath = await this.documents.softDeleteStoredDoc(
                    collection,
                    docId,
                    deletedAt
                )
                await this.publishDocumentEvent(collection, {
                    ts: deletedAt,
                    action: 'delete',
                    id: docId,
                    doc: await this.encodeEncrypted(collection, existing.data),
                    createdAt: existing.createdAt,
                    updatedAt: existing.updatedAt
                })
                await this.runSyncTask(collection, docId, 'delete', deletedPath, async () => {
                    await this.syncDelete({
                        operation: 'delete',
                        collection,
                        docId,
                        path: deletedPath
                    })
                })
                await this.invalidateQueryCache(collection)
            } finally {
                await this.locks.release(collection, docId, owner)
            }
        })
    }
    /** @param {string} collection @param {TTID} docId @returns {Promise<TTID>} */
    async restoreDocument(collection, docId) {
        validateDocId(docId)
        if (this.wormEnabled()) throw new Error('Restore is not allowed in WORM mode')
        await this.requireCollection(collection)
        await this.assertNoLegacyWormArtifacts(collection)
        return await this.withCollectionWriteLock(collection, async () => {
            const owner = Bun.randomUUIDv7()
            if (!(await this.locks.acquire(collection, docId, owner)))
                throw new Error(`Unable to acquire filesystem lock for ${docId}`)
            try {
                if (await this.documents.readStoredDoc(collection, docId)) {
                    throw new Error(`Cannot restore document because it already exists: ${docId}`)
                }
                const deleted = await this.documents.readDeletedDoc(collection, docId)
                if (!deleted) throw new Error(`Deleted document not found: ${docId}`)
                const restoredPath = await this.documents.restoreStoredDoc(
                    collection,
                    docId,
                    Date.now()
                )
                await this.rebuildIndexes(collection, docId, deleted.data)
                await this.publishDocumentEvent(collection, {
                    ts: Date.now(),
                    action: 'insert',
                    id: docId,
                    doc: await this.encodeEncrypted(collection, deleted.data)
                })
                await this.runSyncTask(collection, docId, 'restore', restoredPath, async () => {
                    await this.syncWrite({
                        operation: 'restore',
                        collection,
                        docId,
                        path: restoredPath,
                        data: deleted.data
                    })
                })
                await this.invalidateQueryCache(collection)
                return docId
            } finally {
                await this.locks.release(collection, docId, owner)
            }
        })
    }
    /** @param {string} collection @param {TTID} docId @param {boolean} [onlyId] @returns {any} */
    getDoc(collection, docId, onlyId = false) {
        validateDocId(docId)
        const engine = this
        return {
            async *[Symbol.asyncIterator]() {
                const doc = await this.once()
                if (Object.keys(doc).length > 0) yield onlyId ? Object.keys(doc).shift() : doc
                for await (const event of engine.events.listen(collection)) {
                    if (event.action !== 'insert' || event.id !== docId || !event.doc) continue
                    const doc = await engine.decodeEncrypted(collection, event.doc)
                    yield onlyId ? event.id : { [event.id]: doc }
                }
            },
            /** @returns {Promise<Record<TTID, Record<string, any>>>} */
            async once() {
                await engine.requireCollection(collection)
                await engine.assertNoLegacyWormArtifacts(collection)
                const stored = await engine.documents.readStoredDoc(collection, docId)
                if (!stored) return {}
                const data = /** @type {Record<string, any>} */ (
                    await materializeDoc(collection, stored.data)
                )
                return { [docId]: data }
            },
            async *onDelete() {
                await engine.requireCollection(collection)
                for await (const event of engine.events.listen(collection)) {
                    if (event.action === 'delete' && event.id === docId) yield event.id
                }
            }
        }
    }
    /** @param {string} collection @param {TTID} docId @param {boolean} [onlyId] @returns {Promise<Record<TTID, Record<string, any>> | TTID | null>} */
    async getLatest(collection, docId, onlyId = false) {
        validateDocId(docId)
        await this.requireCollection(collection)
        await this.assertNoLegacyWormArtifacts(collection)
        const stored = await this.documents.readStoredDoc(collection, docId)
        if (!stored) return onlyId ? null : {}
        if (onlyId) return stored.id
        const data = /** @type {Record<string, any>} */ (
            await materializeDoc(collection, stored.data)
        )
        return { [stored.id]: data }
    }
    /** @param {string} collection @param {StoreQuery | undefined} query @returns {any} */
    findDocs(collection, query) {
        const engine = this
        const collectDocs = async function* () {
            const docs = await engine.docResults(collection, query)
            for (const doc of docs) {
                const result = engine.queryEngine.processDoc(doc, query)
                if (result !== undefined) yield result
            }
        }
        return {
            async *[Symbol.asyncIterator]() {
                for await (const result of collectDocs()) yield result
                for await (const event of engine.events.listen(collection)) {
                    if (event.action !== 'insert' || !event.doc) continue
                    const doc = await engine.decodeEncrypted(collection, event.doc)
                    const stored = await engine.documents.readStoredDoc(collection, event.id)
                    if (!stored || !engine.queryEngine.matchesQuery(event.id, doc, query, stored))
                        continue
                    const processed = engine.queryEngine.processDoc({ [event.id]: doc }, query)
                    if (processed !== undefined) yield processed
                }
            },
            async *collect() {
                for await (const result of collectDocs()) yield result
            },
            async *onDelete() {
                await engine.requireCollection(collection)
                for await (const event of engine.events.listen(collection)) {
                    if (event.action !== 'delete' || !event.doc) continue
                    const doc = await engine.decodeEncrypted(collection, event.doc)
                    if (
                        !engine.queryEngine.matchesQuery(event.id, doc, query, {
                            createdAt: event.createdAt ?? event.ts,
                            updatedAt: event.updatedAt ?? event.ts
                        })
                    )
                        continue
                    yield event.id
                }
            }
        }
    }
    /** @param {string} collection @param {StoreQuery | undefined} query @returns {any} */
    findDeletedDocs(collection, query) {
        const engine = this
        const collectDocs = async function* () {
            const docs = await engine.deletedDocResults(collection, query)
            for (const doc of docs) {
                const result = engine.queryEngine.processDoc(doc, query)
                if (result !== undefined) yield result
            }
        }
        return {
            async *[Symbol.asyncIterator]() {
                yield* collectDocs()
            },
            async *collect() {
                yield* collectDocs()
            }
        }
    }
    /** @param {string} collection @returns {AsyncGenerator<Record<string, any>, void, unknown>} */
    async *exportBulkData(collection) {
        const ids = await this.listQueryableDocIds(collection)
        for (const id of ids) {
            const stored = await this.documents.readStoredDoc(collection, id)
            if (!stored) continue
            yield /** @type {Record<string, any>} */ (await materializeDoc(collection, stored.data))
        }
    }
    /** @param {StoreJoin} join @returns {Promise<any>} */
    async joinDocs(join) {
        const leftDocs = await this.docResults(join.$leftCollection)
        const rightDocs = await this.docResults(join.$rightCollection)
        /** @type {Record<string, Record<string, any>>} */
        const docs = {}
        /** @type {Record<string, (leftVal: unknown, rightVal: unknown) => boolean>} */
        const compareMap = {
            $eq: (leftVal, rightVal) => leftVal === rightVal,
            $ne: (leftVal, rightVal) => leftVal !== rightVal,
            $gt: (leftVal, rightVal) => Number(leftVal) > Number(rightVal),
            $lt: (leftVal, rightVal) => Number(leftVal) < Number(rightVal),
            $gte: (leftVal, rightVal) => Number(leftVal) >= Number(rightVal),
            $lte: (leftVal, rightVal) => Number(leftVal) <= Number(rightVal)
        }
        for (const leftEntry of leftDocs) {
            const [leftId, leftData] = Object.entries(leftEntry)[0]
            for (const rightEntry of rightDocs) {
                const [rightId, rightData] = Object.entries(rightEntry)[0]
                let matched = false
                for (const field in join.$on) {
                    const operand = join.$on[field]
                    if (!operand) continue
                    for (const opKey of Object.keys(compareMap)) {
                        const rightField = operand[/** @type {keyof typeof operand} */ (opKey)]
                        if (!rightField) continue
                        const leftValue = this.queryEngine.getValueByPath(leftData, String(field))
                        const rightValue = this.queryEngine.getValueByPath(
                            rightData,
                            String(rightField)
                        )
                        if (compareMap[opKey]?.(leftValue, rightValue)) matched = true
                    }
                }
                if (!matched) continue
                switch (join.$mode) {
                    case 'inner':
                        docs[`${leftId}, ${rightId}`] = { ...leftData, ...rightData }
                        break
                    case 'left':
                        docs[`${leftId}, ${rightId}`] = leftData
                        break
                    case 'right':
                        docs[`${leftId}, ${rightId}`] = rightData
                        break
                    case 'outer':
                        docs[`${leftId}, ${rightId}`] = { ...leftData, ...rightData }
                        break
                }
                let projected = docs[`${leftId}, ${rightId}`]
                if (join.$select?.length) {
                    projected = this.queryEngine.selectValues(join.$select, projected)
                }
                if (join.$rename) {
                    projected = this.queryEngine.renameFields(join.$rename, projected)
                }
                docs[`${leftId}, ${rightId}`] = projected
                if (join.$limit && Object.keys(docs).length >= join.$limit) break
            }
            if (join.$limit && Object.keys(docs).length >= join.$limit) break
        }
        if (join.$groupby) {
            /** @type {Record<string, Record<string, Record<string, any>>>} */
            const groupedDocs = {}
            for (const ids in docs) {
                const data = docs[ids]
                const key = String(data[join.$groupby])
                if (!groupedDocs[key]) groupedDocs[key] = {}
                groupedDocs[key][ids] = data
            }
            if (join.$onlyIds) {
                /** @type {Record<string, string[]>} */
                const groupedIds = {}
                for (const key in groupedDocs)
                    groupedIds[key] = Object.keys(groupedDocs[key]).flat()
                return groupedIds
            }
            return groupedDocs
        }
        if (join.$onlyIds) return Array.from(new Set(Object.keys(docs).flat()))
        return docs
    }
}
