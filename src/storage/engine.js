import path from 'node:path'
import { validateCollectionName } from '../core/collection.js'
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
 * @typedef {import('../replication/sync.js').FyloWormWriteSyncInfo} FyloWormWriteSyncInfo
 * @typedef {import('../replication/sync.js').FyloWormDeleteSyncInfo} FyloWormDeleteSyncInfo
 * @typedef {import('../observability/events.js').FyloEventHandler} FyloEventHandler
 * @typedef {import('./types.js').StorageEngine} StorageEngine
 * @typedef {import('./types.js').LockManager} LockManager
 * @typedef {import('./types.js').EventBus<Record<string, any>>} EventBus
 * @typedef {import('./types.js').CollectionInspectResult} CollectionInspectResult
 * @typedef {import('./types.js').CollectionRebuildResult} CollectionRebuildResult
 * @typedef {import('./types.js').FilesystemEvent<Record<string, any>>} FilesystemEvent
 * @typedef {import('./types.js').StoredDoc<Record<string, any>>} StoredDoc
 * @typedef {import('./types.js').StoredHead} StoredHead
 * @typedef {import('./types.js').StoredVersionMeta} StoredVersionMeta
 * @typedef {import('./types.js').PrefixIndexStore} PrefixIndexStore
 * @typedef {import('../queue/local.js').LocalQueue} LocalQueue
 * @typedef {import('../query/types.js').StoreJoin<Record<string, any>, Record<string, any>>} StoreJoin
 * @typedef {import('../query/types.js').StoreQuery<Record<string, any>>} StoreQuery
 */

/**
 * Low-level filesystem storage engine for collections, documents, indexes,
 * events, locks, WORM heads, and version metadata.
 */
export class FilesystemEngine {
    /** @type {string} */
    root
    /** @type {'filesystem'} */
    kind = 'filesystem'
    /** @type {Map<string, Promise<void>>} */
    writeLanes = new Map()
    /** @type {StorageEngine} */
    storage
    /** @type {LockManager} */
    locks
    /** @type {EventBus} */
    events
    /** @type {LocalQueue | undefined} */
    queue
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
     * @param {{ sync?: FyloSyncHooks, syncMode?: FyloSyncMode, worm?: FyloWormOptions, onEvent?: FyloEventHandler, index?: import('./types.js').FyloIndexOptions, queue?: LocalQueue }} [options]
     */
    constructor(
        root = process.env.FYLO_ROOT ?? path.join(process.cwd(), '.fylo-data'),
        options = {}
    ) {
        this.root = root
        this.sync = options.sync
        this.syncMode = resolveSyncMode(options.syncMode)
        this.worm = {
            mode: options.worm?.mode ?? 'off',
            deletePolicy: options.worm?.deletePolicy ?? 'reject'
        }
        this.onEvent = options.onEvent
        this.storage = new FilesystemStorage()
        this.locks = new FilesystemLockManager(this.root, this.storage)
        this.events = new FilesystemEventBus(this.root, this.storage)
        this.queue = options.queue
        this.index =
            options.index?.backend === 's3-client'
                ? new BunS3ClientIndexStore(options.index.s3)
                : new LocalFsPrefixIndexStore(this.collectionRoot.bind(this))
        this.documents = new FilesystemDocuments(
            this.storage,
            this.docsRoot.bind(this),
            this.docPath.bind(this),
            this.headsRoot.bind(this),
            this.headPath.bind(this),
            this.versionsRoot.bind(this),
            this.versionMetaPath.bind(this),
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
    metaRoot(collection) {
        return this.collectionRoot(collection)
    }
    /** @param {string} collection @returns {string} */
    headsRoot(collection) {
        return path.join(this.metaRoot(collection), 'heads')
    }
    /** @param {string} collection @returns {string} */
    versionsRoot(collection) {
        return path.join(this.metaRoot(collection), 'versions')
    }
    /** @param {string} collection @param {string} lineageId @returns {string} */
    headPath(collection, lineageId) {
        const headsRoot = this.headsRoot(collection)
        const target = path.join(headsRoot, `${lineageId}.json`)
        assertPathInside(headsRoot, target)
        return target
    }
    /** @param {string} collection @param {TTID} docId @returns {string} */
    versionMetaPath(collection, docId) {
        validateDocId(docId)
        const versionsRoot = this.versionsRoot(collection)
        const target = path.join(versionsRoot, `${docId}.meta.json`)
        assertPathInside(versionsRoot, target)
        return target
    }
    /** @param {string} collection @param {TTID} docId @returns {string} */
    docPath(collection, docId) {
        validateDocId(docId)
        const docsRoot = this.docsRoot(collection)
        const target = path.join(docsRoot, docId.slice(0, 2), `${docId}.json`)
        assertPathInside(docsRoot, target)
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
                console.error(
                    new FyloSyncError({
                        collection,
                        docId,
                        operation,
                        path: targetPath,
                        cause
                    })
                )
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
        await this.storage.mkdir(this.metaRoot(collection))
        await this.storage.mkdir(this.docsRoot(collection))
        await this.index.ensureCollection(collection)
        if (this.wormEnabled()) {
            await this.storage.mkdir(this.headsRoot(collection))
            await this.storage.mkdir(this.versionsRoot(collection))
        }
    }
    /** @returns {boolean} */
    wormEnabled() {
        return this.worm.mode === 'append-only'
    }
    /** @param {TTID} docId @returns {string} */
    inferredLineageBucket(docId) {
        return docId.split('-')[0] ?? docId
    }
    /** @param {string} collection @returns {Promise<void>} */
    async resetIndex(collection) {
        await this.index.resetCollection(collection)
    }
    /** @param {string} collection @returns {Promise<TTID[]>} */
    async listQueryableDocIds(collection) {
        if (!this.wormEnabled()) return await this.documents.listDocIds(collection)
        return await this.documents.listActiveDocIds(collection)
    }
    /** @param {string} collection @param {TTID} docId @returns {Promise<StoredHead | null>} */
    async resolveHead(collection, docId) {
        if (!this.wormEnabled()) {
            const existing = await this.documents.readStoredDoc(collection, docId)
            if (!existing) return null
            return {
                version: 1,
                lineageId: docId,
                currentVersionId: docId,
                deleted: false
            }
        }
        return await this.documents.resolveHead(collection, docId)
    }
    /** @param {string} collection @param {TTID} docId @param {Record<string, any>} doc @returns {Promise<{ lineageId: string, data: Record<string, any>, headPath: string }>} */
    async initializeWormVersion(collection, docId, doc) {
        const existingMeta = await this.documents.readVersionMeta(collection, docId)
        const lineageId = existingMeta?.lineageId ?? docId
        await this.documents.writeVersionMeta(collection, {
            version: 1,
            versionId: docId,
            lineageId,
            previousVersionId: existingMeta?.previousVersionId,
            supersededAt: existingMeta?.supersededAt,
            deletedAt: existingMeta?.deletedAt
        })
        await this.documents.writeHead(collection, {
            version: 1,
            lineageId,
            currentVersionId: docId,
            deleted: false
        })
        return { lineageId, data: doc, headPath: this.headPath(collection, lineageId) }
    }
    /** @param {string} collection @param {string} lineageId @param {TTID} headDocId @param {'create' | 'advance'} headOperation @returns {FyloWormWriteSyncInfo} */
    buildWormWriteSyncInfo(collection, lineageId, headDocId, headOperation) {
        return {
            lineageId,
            headOperation,
            headDocId,
            headPath: this.headPath(collection, lineageId)
        }
    }
    /** @param {{ collection: string, lineageId: string, headDocId: TTID, deleteMode: 'physical' | 'tombstone', versionPath?: string }} args @returns {FyloWormDeleteSyncInfo} */
    buildWormDeleteSyncInfo(args) {
        return {
            lineageId: args.lineageId,
            headOperation: 'delete',
            headDocId: args.headDocId,
            headPath: this.headPath(args.collection, args.lineageId),
            deleteMode: args.deleteMode,
            versionPath: args.versionPath
        }
    }
    /**
     * Advances a lineage from one document version to another.
     * @param {string} collection
     * @param {TTID} oldId
     * @param {TTID} newId
     * @param {Record<string, any>} nextDoc
     * @param {Record<string, any>} oldDoc
     * @returns {Promise<TTID>}
     */
    async advanceDocumentVersion(collection, oldId, newId, nextDoc, oldDoc) {
        validateDocId(oldId)
        validateDocId(newId)
        return await this.withCollectionWriteLock(collection, async () => {
            const owner = Bun.randomUUIDv7()
            if (!(await this.locks.acquire(collection, oldId, owner)))
                throw new Error(`Unable to acquire filesystem lock for ${oldId}`)
            const oldPath = this.docPath(collection, oldId)
            try {
                const existing =
                    oldDoc ?? (await this.documents.readStoredDoc(collection, oldId))?.data
                if (!existing) return oldId
                const newPath = this.docPath(collection, newId)
                await this.removeIndexes(collection, oldId, existing)
                if (!this.wormEnabled()) {
                    await this.documents.removeStoredDoc(collection, oldId)
                }
                await this.publishDocumentEvent(collection, {
                    ts: Date.now(),
                    action: 'delete',
                    id: oldId,
                    doc: await this.encodeEncrypted(collection, existing)
                })
                await this.documents.writeStoredDoc(collection, newId, nextDoc)
                await this.rebuildIndexes(collection, newId, nextDoc)
                /** @type {FyloWormWriteSyncInfo | undefined} */
                let wormWriteInfo
                if (this.wormEnabled()) {
                    const oldMeta = await this.documents.readVersionMeta(collection, oldId)
                    const lineageId = oldMeta?.lineageId ?? oldId
                    await this.documents.writeVersionMeta(collection, {
                        version: 1,
                        versionId: oldId,
                        lineageId,
                        previousVersionId: oldMeta?.previousVersionId,
                        supersededAt: Date.now(),
                        deletedAt: oldMeta?.deletedAt
                    })
                    await this.documents.writeVersionMeta(collection, {
                        version: 1,
                        versionId: newId,
                        lineageId,
                        previousVersionId: oldId
                    })
                    await this.documents.writeHead(collection, {
                        version: 1,
                        lineageId,
                        currentVersionId: newId,
                        deleted: false
                    })
                    wormWriteInfo = this.buildWormWriteSyncInfo(
                        collection,
                        lineageId,
                        newId,
                        'advance'
                    )
                }
                await this.publishDocumentEvent(collection, {
                    ts: Date.now(),
                    action: 'insert',
                    id: newId,
                    doc: await this.encodeEncrypted(collection, nextDoc)
                })
                await this.runSyncTask(collection, newId, 'patch', newPath, async () => {
                    if (!this.wormEnabled()) {
                        await this.syncDelete({
                            operation: 'patch',
                            collection,
                            docId: oldId,
                            path: oldPath
                        })
                    }
                    await this.syncWrite({
                        operation: 'patch',
                        collection,
                        docId: newId,
                        previousDocId: oldId,
                        path: newPath,
                        data: nextDoc,
                        worm: wormWriteInfo
                    })
                })
                return newId
            } finally {
                await this.locks.release(collection, oldId, owner)
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
    }
    /** @param {string} collection @returns {Promise<void>} */
    async dropCollection(collection) {
        await this.storage.rmdir(this.collectionRoot(collection))
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
                indexedDocs: 0,
                headFiles: 0,
                activeHeads: 0,
                deletedHeads: 0,
                versionMetas: 0
            }
        }
        const [docIds, indexedDocs, headFiles, versionFiles] = await Promise.all([
            this.documents.listDocIds(collection),
            this.index.countDocuments(collection),
            this.storage.list(this.headsRoot(collection)),
            this.storage.list(this.versionsRoot(collection))
        ])
        let headCount = 0
        let activeHeads = 0
        let deletedHeads = 0
        for (const headFile of headFiles) {
            if (!headFile.endsWith('.json')) continue
            headCount++
            const head = JSON.parse(await this.storage.read(headFile))
            if (head.deleted) deletedHeads++
            else activeHeads++
        }
        const versionMetas = versionFiles.filter((file) => file.endsWith('.meta.json')).length
        return {
            collection,
            exists: true,
            worm: this.wormEnabled() || headCount > 0 || versionMetas > 0,
            docsStored: docIds.length,
            indexedDocs,
            headFiles: headCount,
            activeHeads,
            deletedHeads,
            versionMetas
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
        const candidateIds = await this.queryEngine.candidateDocIdsForQuery(collection, query)
        const ids = candidateIds
            ? Array.from(candidateIds)
            : await this.listQueryableDocIds(collection)
        const limit = query?.$limit
        /** @type {Array<Record<string, Record<string, any>>>} */
        const results = []
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
            if (!this.queryEngine.matchesQuery(id, data, query)) continue
            results.push({ [id]: data })
            if (limit && results.length >= limit) break
        }
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
        return await this.withCollectionWriteLock(collection, async () => {
            await this.ensureCollection(collection)
            const docIds = await this.documents.listDocIds(collection)
            const docs = new Map()
            for (const docId of docIds) {
                const stored = await this.documents.readStoredDoc(collection, docId)
                if (stored) docs.set(docId, stored)
            }
            let indexedDocs = 0
            let headsRebuilt = 0
            let versionMetasRebuilt = 0
            let staleHeadsRemoved = 0
            let staleVersionMetasRemoved = 0
            await this.resetIndex(collection)
            if (!this.wormEnabled()) {
                for (const [docId, stored] of docs) {
                    await this.index.putDocument(collection, docId, stored.data)
                    indexedDocs++
                }
                emitFyloEvent(this.onEvent, {
                    type: 'index.rebuilt',
                    collection,
                    docsScanned: docs.size,
                    indexedDocs,
                    worm: false
                })
                return {
                    collection,
                    worm: false,
                    docsScanned: docs.size,
                    indexedDocs,
                    headsRebuilt,
                    versionMetasRebuilt,
                    staleHeadsRemoved,
                    staleVersionMetasRemoved
                }
            }
            /** @type {Map<string, Array<{ docId: TTID, stored: StoredDoc, meta: StoredVersionMeta | null }>>} */
            const grouped = new Map()
            for (const [docId, stored] of docs) {
                const meta = await this.documents.readVersionMeta(collection, docId)
                const bucket = meta?.lineageId
                    ? this.inferredLineageBucket(meta.lineageId)
                    : this.inferredLineageBucket(docId)
                const entries = grouped.get(bucket) ?? []
                entries.push({ docId, stored, meta })
                grouped.set(bucket, entries)
            }
            /** @type {TTID[]} */
            const activeDocIds = []
            /** @type {Set<string>} */
            const validLineageIds = new Set()
            /** @type {Set<TTID>} */
            const validVersionIds = new Set()
            for (const entries of grouped.values()) {
                entries.sort((left, right) => left.stored.updatedAt - right.stored.updatedAt)
                const lineageId =
                    entries.find((entry) => entry.meta?.lineageId)?.meta?.lineageId ??
                    entries[0]?.docId
                if (!lineageId) continue
                validLineageIds.add(lineageId)
                const existingHead = await this.documents.readHead(collection, lineageId)
                const currentHead = entries.at(-1)
                if (!currentHead) continue
                for (let index = 0; index < entries.length; index++) {
                    const entry = entries[index]
                    const next = entries[index + 1]
                    validVersionIds.add(entry.docId)
                    await this.documents.writeVersionMeta(collection, {
                        version: 1,
                        versionId: entry.docId,
                        lineageId,
                        previousVersionId: index > 0 ? entries[index - 1].docId : undefined,
                        supersededAt: next ? next.stored.updatedAt : undefined,
                        deletedAt:
                            existingHead?.deleted && currentHead.docId === entry.docId
                                ? existingHead.deletedAt
                                : entry.meta?.deletedAt
                    })
                    versionMetasRebuilt++
                }
                const headMeta = await this.documents.readVersionMeta(collection, currentHead.docId)
                const deleted = Boolean(existingHead?.deleted || headMeta?.deletedAt)
                const deletedAt = existingHead?.deletedAt ?? headMeta?.deletedAt
                await this.documents.writeHead(collection, {
                    version: 1,
                    lineageId,
                    currentVersionId: currentHead.docId,
                    deleted,
                    deletedAt
                })
                headsRebuilt++
                if (!deleted) activeDocIds.push(currentHead.docId)
            }
            for (const headFile of await this.storage.list(this.headsRoot(collection))) {
                if (!headFile.endsWith('.json')) continue
                const lineageId = path.basename(headFile, '.json')
                if (validLineageIds.has(lineageId)) continue
                await this.storage.delete(headFile)
                staleHeadsRemoved++
            }
            for (const versionFile of await this.storage.list(this.versionsRoot(collection))) {
                if (!versionFile.endsWith('.meta.json')) continue
                const versionId = path.basename(versionFile, '.meta.json')
                if (validVersionIds.has(versionId)) continue
                await this.storage.delete(versionFile)
                staleVersionMetasRemoved++
            }
            for (const docId of activeDocIds) {
                const stored = docs.get(docId)
                if (!stored) continue
                await this.index.putDocument(collection, docId, stored.data)
                indexedDocs++
            }
            emitFyloEvent(this.onEvent, {
                type: 'index.rebuilt',
                collection,
                docsScanned: docs.size,
                indexedDocs,
                worm: true
            })
            return {
                collection,
                worm: true,
                docsScanned: docs.size,
                indexedDocs,
                headsRebuilt,
                versionMetasRebuilt,
                staleHeadsRemoved,
                staleVersionMetasRemoved
            }
        })
    }
    /** @param {string} collection @param {TTID} docId @param {Record<string, any>} doc @returns {Promise<void>} */
    async putDocument(collection, docId, doc) {
        validateDocId(docId)
        await this.withCollectionWriteLock(collection, async () => {
            const owner = Bun.randomUUIDv7()
            if (!(await this.locks.acquire(collection, docId, owner)))
                throw new Error(`Unable to acquire filesystem lock for ${docId}`)
            const targetPath = this.docPath(collection, docId)
            /** @type {{ lineageId: string, headPath: string } | undefined} */
            let wormInfo
            try {
                const existing = await this.documents.readStoredDoc(collection, docId)
                if (existing) await this.removeIndexes(collection, docId, existing.data)
                await this.documents.writeStoredDoc(collection, docId, doc)
                if (this.wormEnabled()) {
                    const initialized = await this.initializeWormVersion(collection, docId, doc)
                    wormInfo = {
                        lineageId: initialized.lineageId,
                        headPath: initialized.headPath
                    }
                }
                await this.rebuildIndexes(collection, docId, doc)
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
                        data: doc,
                        worm: wormInfo
                            ? this.buildWormWriteSyncInfo(
                                  collection,
                                  wormInfo.lineageId,
                                  docId,
                                  'create'
                              )
                            : undefined
                    })
                })
            } finally {
                await this.locks.release(collection, docId, owner)
            }
        })
    }
    /** @param {string} collection @param {TTID} oldId @param {TTID} newId @param {Record<string, any>} patch @param {Record<string, any>} oldDoc @returns {Promise<TTID>} */
    async patchDocument(collection, oldId, newId, patch, oldDoc) {
        const existing = oldDoc ?? (await this.documents.readStoredDoc(collection, oldId))?.data
        if (!existing) return oldId
        const nextDoc = { ...existing, ...patch }
        return await this.advanceDocumentVersion(collection, oldId, newId, nextDoc, existing)
    }
    /** @param {string} collection @param {TTID} oldId @param {TTID} newId @param {Record<string, any>} doc @param {Record<string, any>} [oldDoc] @returns {Promise<TTID>} */
    async replaceDocumentVersion(collection, oldId, newId, doc, oldDoc) {
        if (oldDoc) return await this.advanceDocumentVersion(collection, oldId, newId, doc, oldDoc)
        const stored = await this.documents.readStoredDoc(collection, oldId)
        if (!stored) return oldId
        return await this.advanceDocumentVersion(collection, oldId, newId, doc, stored.data)
    }
    /** @param {string} collection @param {TTID} docId @returns {Promise<void>} */
    async deleteDocument(collection, docId) {
        validateDocId(docId)
        if (this.wormEnabled() && this.worm.deletePolicy === 'reject')
            throw new Error('Delete is not allowed in WORM mode')
        await this.withCollectionWriteLock(collection, async () => {
            const owner = Bun.randomUUIDv7()
            if (!(await this.locks.acquire(collection, docId, owner)))
                throw new Error(`Unable to acquire filesystem lock for ${docId}`)
            const targetPath = this.docPath(collection, docId)
            try {
                const existing = await this.documents.readStoredDoc(collection, docId)
                if (!existing) return
                if (this.wormEnabled() && this.worm.deletePolicy === 'tombstone') {
                    const head = await this.documents.resolveHead(collection, docId)
                    const lineageId = head?.lineageId ?? docId
                    const headPath = this.headPath(collection, lineageId)
                    const deletedAt = Date.now()
                    await this.removeIndexes(collection, docId, existing.data)
                    const existingMeta = await this.documents.readVersionMeta(collection, docId)
                    await this.documents.writeVersionMeta(collection, {
                        version: 1,
                        versionId: docId,
                        lineageId,
                        previousVersionId: existingMeta?.previousVersionId,
                        supersededAt: existingMeta?.supersededAt,
                        deletedAt
                    })
                    await this.documents.writeHead(collection, {
                        version: 1,
                        lineageId,
                        currentVersionId: docId,
                        deleted: true,
                        deletedAt
                    })
                    await this.publishDocumentEvent(collection, {
                        ts: Date.now(),
                        action: 'delete',
                        id: docId,
                        doc: await this.encodeEncrypted(collection, existing.data)
                    })
                    await this.runSyncTask(collection, docId, 'delete', targetPath, async () => {
                        await this.syncDelete({
                            operation: 'delete',
                            collection,
                            docId,
                            path: headPath,
                            worm: this.buildWormDeleteSyncInfo({
                                collection,
                                lineageId,
                                headDocId: docId,
                                deleteMode: 'tombstone',
                                versionPath: targetPath
                            })
                        })
                    })
                    return
                }
                await this.removeIndexes(collection, docId, existing.data)
                await this.documents.removeStoredDoc(collection, docId)
                await this.publishDocumentEvent(collection, {
                    ts: Date.now(),
                    action: 'delete',
                    id: docId,
                    doc: await this.encodeEncrypted(collection, existing.data)
                })
                await this.runSyncTask(collection, docId, 'delete', targetPath, async () => {
                    await this.syncDelete({
                        operation: 'delete',
                        collection,
                        docId,
                        path: targetPath
                    })
                })
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
                const stored = await engine.documents.readStoredDoc(collection, docId)
                if (!stored) return {}
                const data = /** @type {Record<string, any>} */ (
                    await materializeDoc(collection, stored.data)
                )
                return { [docId]: data }
            },
            async *onDelete() {
                for await (const event of engine.events.listen(collection)) {
                    if (event.action === 'delete' && event.id === docId) yield event.id
                }
            }
        }
    }
    /** @param {string} collection @param {TTID} docId @param {boolean} [onlyId] @returns {Promise<Record<TTID, Record<string, any>> | TTID | null>} */
    async getLatest(collection, docId, onlyId = false) {
        validateDocId(docId)
        const head = await this.resolveHead(collection, docId)
        if (!head || head.deleted) return onlyId ? null : {}
        const stored = await this.documents.readStoredDoc(collection, head.currentVersionId)
        if (!stored) return onlyId ? null : {}
        if (onlyId) return stored.id
        const data = /** @type {Record<string, any>} */ (
            await materializeDoc(collection, stored.data)
        )
        return { [stored.id]: data }
    }
    /** @param {string} collection @param {TTID} docId @returns {Promise<any[]>} */
    async getHistory(collection, docId) {
        validateDocId(docId)
        const head = await this.resolveHead(collection, docId)
        if (!head) return []
        const history = []
        let currentId = head.currentVersionId
        while (currentId) {
            const stored = await this.documents.readStoredDoc(collection, currentId)
            if (!stored) break
            const meta = this.wormEnabled()
                ? await this.documents.readVersionMeta(collection, currentId)
                : null
            history.push({
                ...stored,
                lineageId: meta?.lineageId ?? head.lineageId,
                previousVersionId: meta?.previousVersionId,
                supersededAt: meta?.supersededAt,
                isHead: currentId === head.currentVersionId,
                deleted: Boolean(
                    currentId === head.currentVersionId && (head.deleted || meta?.deletedAt)
                ),
                deletedAt:
                    currentId === head.currentVersionId
                        ? (head.deletedAt ?? meta?.deletedAt)
                        : undefined
            })
            currentId = meta?.previousVersionId ?? ''
        }
        return history
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
                    if (!engine.queryEngine.matchesQuery(event.id, doc, query)) continue
                    const processed = engine.queryEngine.processDoc({ [event.id]: doc }, query)
                    if (processed !== undefined) yield processed
                }
            },
            async *collect() {
                for await (const result of collectDocs()) yield result
            },
            async *onDelete() {
                for await (const event of engine.events.listen(collection)) {
                    if (event.action !== 'delete' || !event.doc) continue
                    const doc = await engine.decodeEncrypted(collection, event.doc)
                    if (!engine.queryEngine.matchesQuery(event.id, doc, query)) continue
                    yield event.id
                }
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
