import path from 'node:path'
import { readdir } from 'node:fs/promises'
import TTID from '@d31ma/ttid'
import { Parser } from '../query/parser.js'
import { FyloAuthError } from '../security/auth.js'
import { Cipher } from '../security/cipher.js'
import { FilesystemEngine } from '../storage/engine.js'
import { emitFyloEvent } from '../observability/events.js'
import { LocalQueue } from '../queue/local.js'
import { validateDocId } from '../core/doc-id.js'
import { validateAgainstHead } from '../schema/validation.js'
import { materializeDoc, materializeEnvelope } from '../schema/migrate.js'
import { schemaEnv, syncChexSchemaEnv } from '../schema/env.js'
import { loadHeadSchema } from '../schema/versioning.js'
import { authorizeOperation, isDocVisible } from '../security/rules/engine.js'
import { loadRules } from '../security/rules/loader.js'
import {
    normalizeImportOptions,
    assertImportUrlAllowed,
    redactImportUrl,
    tlsCheckServerIdentity
} from '../security/import-guard.js'
import '../core/extensions.js'

/**
 * @typedef {import('../security/auth.js').FyloAuthAction} FyloAuthAction
 * @typedef {import('../security/auth.js').FyloAuthContext} FyloAuthContext
 * @typedef {import('../replication/sync.js').FyloOptions<Record<string, any>>} FyloOptions
 * @typedef {import('../replication/sync.js').FyloSyncMode} FyloSyncMode
 * @typedef {import('../replication/sync.js').FyloSyncHooks<Record<string, any>>} FyloSyncHooks
 * @typedef {import('../replication/sync.js').FyloWriteSyncEvent<Record<string, any>>} FyloWriteSyncEvent
 * @typedef {import('../replication/sync.js').FyloDeleteSyncEvent} FyloDeleteSyncEvent
 * @typedef {import('../replication/sync.js').FyloWormMode} FyloWormMode
 * @typedef {import('../replication/sync.js').FyloWormOptions} FyloWormOptions
 * @typedef {import('../replication/sync.js').FyloWormWriteSyncInfo} FyloWormWriteSyncInfo
 * @typedef {import('../replication/sync.js').FyloWormDeleteSyncInfo} FyloWormDeleteSyncInfo
 * @typedef {import('../observability/events.js').FyloEvent} FyloEvent
 * @typedef {import('../observability/events.js').FyloEventHandler} FyloEventHandler
 * @typedef {import('../query/types.js').StoreDelete<Record<string, any>>} StoreDelete
 * @typedef {import('../query/types.js').StoreInsert<Record<string, any>>} StoreInsert
 * @typedef {import('../query/types.js').StoreJoin<Record<string, any>, Record<string, any>>} StoreJoin
 * @typedef {import('../query/types.js').StoreQuery<Record<string, any>>} StoreQuery
 * @typedef {import('../query/types.js').StoreUpdate<Record<string, any>>} StoreUpdate
 * @typedef {import('../types/vendor.js').TTID} TTIDValue
 * @typedef {import('../storage/types.js').CollectionInspectResult} CollectionInspectResult
 * @typedef {import('../storage/types.js').CollectionRebuildResult} CollectionRebuildResult
 * @typedef {import('../queue/local.js').LocalQueue} LocalQueueInstance
 * @typedef {import('../types/fylo.js').GetDocResult<Record<string, any>>} GetDocResult
 * @typedef {import('../types/fylo.js').FindDocsResult<Record<string, any>>} FindDocsResult
 * @typedef {import('../types/fylo.js').JoinDocsResult<Record<string, any>, Record<string, any>>} JoinDocsResult
 */

/**
 * @typedef {import('../security/import-guard.js').ImportBulkDataOptions} ImportBulkDataOptions
 */

/**
 * @typedef {object} FyloHistoryEntry
 * @property {TTIDValue} id
 * @property {number} createdAt
 * @property {number} updatedAt
 * @property {Record<string, any>} data
 * @property {TTIDValue} lineageId
 * @property {TTIDValue=} previousVersionId
 * @property {number=} supersededAt
 * @property {boolean} isHead
 * @property {boolean} deleted
 * @property {number=} deletedAt
 */
export default class Fylo {
    /** @type {string | undefined} */
    static LOGGING = process.env.FYLO_LOGGING
    /** @type {number} */
    static MAX_CPUS = navigator.hardwareConcurrency
    /** @type {string | undefined} */
    static STRICT = process.env.FYLO_STRICT
    /** @type {Promise<void>} */
    static ttidLock = Promise.resolve()
    /** Collections whose schema `$encrypted` config has already been loaded. */
    /** @type {Set<string>} */
    static loadedEncryption = new Set()
    /** @type {FilesystemEngine} */
    engine
    /** @type {boolean} */
    rlsEnabled
    /** @type {FyloEventHandler | undefined} */
    onEvent
    /** @type {LocalQueueInstance | undefined} */
    queue
    /** @type {Promise<void>} */
    startup
    /**
     * @param {FyloOptions} [options]
     */
    constructor(options = {}) {
        syncChexSchemaEnv()
        this.rlsEnabled = options.rls === true
        this.onEvent = options.onEvent
        this.queue = options.queue
            ? new LocalQueue({ root: options.root ?? Fylo.defaultRoot() })
            : undefined
        this.engine = new FilesystemEngine(options.root ?? Fylo.defaultRoot(), {
            sync: options.sync,
            syncMode: options.syncMode,
            worm: options.worm,
            index: options.index,
            onEvent: options.onEvent,
            queue: this.queue
        })
        this.startup = this.bootstrapCollectionsFromSchemas()
    }

    /** @returns {Promise<void>} */
    async ready() {
        await this.startup
    }

    /** @returns {Promise<void>} */
    async bootstrapCollectionsFromSchemas() {
        const schemaDir = schemaEnv()
        if (!schemaDir) return
        /** @type {import('node:fs').Dirent[]} */
        let entries = []
        try {
            entries = await readdir(schemaDir, { withFileTypes: true })
        } catch (err) {
            if (/** @type {NodeJS.ErrnoException} */ (err).code !== 'ENOENT') throw err
            return
        }
        for (const entry of entries) {
            if (!entry.isDirectory()) continue
            const collection = entry.name
            if (!(await Bun.file(path.join(schemaDir, collection, 'manifest.json')).exists()))
                continue
            await this.engine.createCollection(collection)
        }
    }

    /** @returns {string} */
    static defaultRoot() {
        return process.env.FYLO_ROOT || path.join(process.cwd(), '.fylo-data')
    }
    /** @returns {FilesystemEngine} */
    static get defaultEngine() {
        return new FilesystemEngine(Fylo.defaultRoot())
    }
    /**
     * Executes a SQL query and returns the results.
     * @param {string} SQL The SQL query to execute.
     * @returns The results of the query.
     */
    async executeSQL(SQL) {
        await this.ready()
        const operationMatch = SQL.match(/^(SELECT|INSERT|UPDATE|DELETE|CREATE|DROP)/i)
        const operation = operationMatch?.[0]?.toUpperCase()
        if (!operation) throw new Error('Missing SQL Operation')
        switch (operation) {
            case 'CREATE':
                return await this.createCollection(
                    /** @type {{ $collection: string }} */ (Parser.parse(SQL)).$collection
                )
            case 'DROP':
                return await this.dropCollection(
                    /** @type {{ $collection: string }} */ (Parser.parse(SQL)).$collection
                )
            case 'SELECT': {
                const query = /** @type {StoreQuery} */ (Parser.parse(SQL))
                if (SQL.includes('JOIN'))
                    return await this.joinDocs(/** @type {StoreJoin} */ (query))
                const selectedCollection = query.$collection
                delete query.$collection
                /** @type {TTIDValue[] | Record<string, any>} */
                let docs = query.$onlyIds ? [] : {}
                for await (const data of this.findDocs(
                    String(selectedCollection),
                    query
                ).collect()) {
                    if (typeof data === 'object')
                        docs = /** @type {{ appendGroup(target: any, value: any): any }} */ (
                            /** @type {unknown} */ (Object)
                        ).appendGroup(docs, data)
                    else docs.push(data)
                }
                return docs
            }
            case 'INSERT': {
                const insert = /** @type {StoreInsert} */ (Parser.parse(SQL))
                const insertCollection = insert.$collection
                delete insert.$collection
                return await this.putData(String(insertCollection), insert.$values)
            }
            case 'UPDATE': {
                const update = /** @type {StoreUpdate} */ (Parser.parse(SQL))
                const updateCol = update.$collection
                delete update.$collection
                return await this.patchDocs(String(updateCol), update)
            }
            case 'DELETE': {
                const del = /** @type {StoreDelete} */ (Parser.parse(SQL))
                const deleteCollection = del.$collection
                delete del.$collection
                return await this.delDocs(String(deleteCollection), del)
            }
            default:
                throw new Error('Invalid Operation')
        }
    }
    /**
     * Creates a new collection on the configured filesystem root.
     * @param {string} collection The name of the collection.
     * @returns {Promise<void>}
     */
    static async createCollection(collection) {
        await Fylo.defaultEngine.createCollection(collection)
    }
    /**
     * Drops an existing collection from the configured filesystem root.
     * @param {string} collection The name of the collection.
     * @returns {Promise<void>}
     */
    static async dropCollection(collection) {
        await Fylo.defaultEngine.dropCollection(collection)
    }
    /** @param {string} collection @returns {Promise<CollectionRebuildResult>} */
    static async rebuildCollection(collection) {
        return await Fylo.defaultEngine.rebuildCollection(collection)
    }
    /** @param {string} collection @returns {Promise<CollectionInspectResult>} */
    static async inspectCollection(collection) {
        return await Fylo.defaultEngine.inspectCollection(collection)
    }
    /** @param {string} collection @returns {Promise<void>} */
    async createCollection(collection) {
        await this.ready()
        return await this.engine.createCollection(collection)
    }
    /** @param {string} collection @returns {Promise<void>} */
    async dropCollection(collection) {
        await this.ready()
        return await this.engine.dropCollection(collection)
    }
    /** @param {string} collection @returns {Promise<CollectionRebuildResult>} */
    async rebuildCollection(collection) {
        await this.ready()
        return await this.engine.rebuildCollection(collection)
    }
    /** @param {string} collection @returns {Promise<CollectionInspectResult>} */
    async inspectCollection(collection) {
        await this.ready()
        return await this.engine.inspectCollection(collection)
    }
    /** @param {FyloAuthContext} auth @returns {AuthenticatedFylo} */
    as(auth) {
        if (!this.rlsEnabled) {
            throw new Error('FYLO RLS is not enabled — pass `rls: true` to the Fylo constructor')
        }
        return new AuthenticatedFylo(this, auth)
    }
    /**
     * Loads encrypted field config from a collection's JSON schema if not already loaded.
     * Reads the `$encrypted` array from the schema and registers fields with Cipher.
     * Auto-configures the Cipher key from `FYLO_ENCRYPTION_KEY` env var on first use.
     */
    /** @param {string} collection @returns {Promise<void>} */
    static async loadEncryption(collection) {
        if (Fylo.loadedEncryption.has(collection)) return
        const schemaDir = schemaEnv()
        if (!schemaDir) {
            Fylo.loadedEncryption.add(collection)
            return
        }
        const schema = await loadHeadSchema(collection, schemaDir)
        if (!schema) {
            Fylo.loadedEncryption.add(collection)
            return
        }
        const encrypted = schema.$encrypted
        if (encrypted !== undefined && !Array.isArray(encrypted))
            throw new Error(`Schema $encrypted for ${collection} must be an array of field names`)
        if (Array.isArray(encrypted) && encrypted.length > 0) {
            if (!encrypted.every((field) => typeof field === 'string' && field.length > 0))
                throw new Error(`Schema $encrypted for ${collection} must only contain strings`)
            if (!Cipher.isConfigured()) {
                const secret = process.env.FYLO_ENCRYPTION_KEY
                if (!secret)
                    throw new Error(
                        'Schema declares $encrypted fields but FYLO_ENCRYPTION_KEY env var is not set'
                    )
                if (secret.length < 32)
                    throw new Error('FYLO_ENCRYPTION_KEY must be at least 32 characters long')
                await Cipher.configure(secret)
            }
            Cipher.registerFields(collection, encrypted)
        }
        Fylo.loadedEncryption.add(collection)
    }
    /** @param {string} collection @param {TTIDValue} _id @param {boolean} [onlyId] @returns {GetDocResult} */
    getDoc(collection, _id, onlyId = false) {
        validateDocId(_id)
        const source = this.ready().then(() => this.engine.getDoc(collection, _id, onlyId))
        return {
            async *[Symbol.asyncIterator]() {
                yield* await source
            },
            async once() {
                return await (await source).once()
            },
            async *onDelete() {
                yield* (await source).onDelete()
            }
        }
    }
    /** @param {string} collection @param {TTIDValue} _id @param {boolean} [onlyId] @returns {Promise<Record<TTIDValue, Record<string, any>> | TTIDValue | null>} */
    async getLatest(collection, _id, onlyId = false) {
        await this.ready()
        validateDocId(_id)
        if (onlyId) return await this.engine.getLatest(collection, _id, true)
        return await this.engine.getLatest(collection, _id)
    }
    /** @param {string} collection @param {TTIDValue} _id @returns {Promise<FyloHistoryEntry[]>} */
    async getHistory(collection, _id) {
        await this.ready()
        validateDocId(_id)
        return await this.engine.getHistory(collection, _id)
    }
    /** @param {string} collection @param {StoreQuery} query @returns {FindDocsResult} */
    findDocs(collection, query) {
        const source = this.ready().then(() => this.engine.findDocs(collection, query))
        return {
            async *[Symbol.asyncIterator]() {
                yield* await source
            },
            async *collect() {
                yield* (await source).collect()
            },
            async *onDelete() {
                yield* (await source).onDelete()
            }
        }
    }
    /** @param {StoreJoin} join @returns {Promise<JoinDocsResult>} */
    async joinDocs(join) {
        await this.ready()
        return await this.engine.joinDocs(join)
    }
    /** @param {string} collection @returns {AsyncGenerator<Record<string, any>, void, unknown>} */
    async *exportBulkData(collection) {
        await this.ready()
        yield* this.engine.exportBulkData(collection)
    }
    /** @param {string} collection @param {URL} url @param {number | ImportBulkDataOptions} [limitOrOptions] @returns {Promise<number>} */
    async importBulkData(collection, url, limitOrOptions) {
        await this.ready()
        const importOptions = normalizeImportOptions(limitOrOptions)
        const limit = importOptions.limit
        if (limit !== undefined && limit <= 0) return 0
        let pin
        try {
            pin = await assertImportUrlAllowed(url, importOptions)
        } catch (err) {
            const message = err instanceof Error ? err.message : String(err)
            /** @type {'protocol' | 'host' | 'private-network'} */
            let reason = 'host'
            if (message.includes('protocol is not allowed')) reason = 'protocol'
            else if (message.includes('host is not allowed')) reason = 'host'
            else if (message.includes('private address')) reason = 'private-network'
            emitFyloEvent(this.onEvent, {
                type: 'import.blocked',
                reason,
                url: redactImportUrl(url),
                detail: message
            })
            throw err
        }
        /** @type {RequestInit & { tls?: { serverName?: string, checkServerIdentity?: Function } }} */
        const fetchInit = { redirect: 'manual' }
        if (pin) {
            fetchInit.headers = { Host: url.host }
            if (url.protocol === 'https:') {
                fetchInit.tls = {
                    serverName: pin.serverName,
                    /** @param {string} _hostname @param {import('node:tls').PeerCertificate} cert */
                    checkServerIdentity: (_hostname, cert) =>
                        tlsCheckServerIdentity(pin.serverName, cert)
                }
            }
        }
        /** @type {URL[]} */
        const fetchTargets = pin ? pin.pinnedUrls : [url]
        /** @type {Response | undefined} */
        let response
        /** @type {unknown} */
        let lastFetchError
        for (let i = 0; i < fetchTargets.length; i++) {
            try {
                response = await fetch(fetchTargets[i], fetchInit)
                break
            } catch (err) {
                lastFetchError = err
                if (i === fetchTargets.length - 1) throw err
            }
        }
        if (!response)
            throw lastFetchError instanceof Error
                ? lastFetchError
                : new Error('Import request failed')
        if (response.status >= 300 && response.status < 400) {
            const location = response.headers.get('location')
            const redactedLocation = location ? redactImportUrl(location) : 'unknown'
            emitFyloEvent(this.onEvent, {
                type: 'import.blocked',
                reason: 'redirect',
                url: redactImportUrl(url),
                detail: `redirect to ${redactedLocation}`
            })
            throw new Error(`Import request redirected to ${redactedLocation}`)
        }
        if (!response.ok) throw new Error(`Import request failed with status ${response.status}`)
        if (!response.headers.get('content-type')?.includes('application/json'))
            throw new Error('Response is not JSON')
        if (!response.body) throw new Error('Response body is empty')
        let count = 0
        let batchNum = 0
        /** @param {Record<string, any>[]} batch @returns {Promise<void>} */
        const flush = async (batch) => {
            if (!batch.length) return
            const items =
                limit !== undefined && count + batch.length > limit
                    ? batch.slice(0, limit - count)
                    : batch
            if (!items.length) return
            batchNum++
            const start = Date.now()
            await this.batchPutData(collection, items)
            count += items.length
            if (count % 10000 === 0) console.log('Count:', count)
            if (Fylo.LOGGING) {
                const bytes = JSON.stringify(items).length
                const elapsed = Date.now() - start
                const bytesPerSec = (bytes / (elapsed / 1000)).toFixed(2)
                console.log(
                    `Batch ${batchNum} of ${bytes} bytes took ${elapsed === Infinity ? 'Infinity' : elapsed}ms (${bytesPerSec} bytes/sec)`
                )
            }
        }
        let isJsonArray = null
        const jsonArrayChunks = []
        let jsonArrayLength = 0
        let pending = new Uint8Array(0)
        /** @type {Record<string, any>[]} */
        let batch = []
        let totalBytes = 0
        for await (const chunk of /** @type {AsyncIterable<Uint8Array>} */ (
            /** @type {unknown} */ (response.body)
        )) {
            totalBytes += chunk.length
            if (totalBytes > importOptions.maxBytes)
                throw new Error(`Import response exceeded ${importOptions.maxBytes} bytes`)
            if (isJsonArray === null) isJsonArray = chunk[0] === 0x5b
            if (isJsonArray) {
                jsonArrayChunks.push(chunk)
                jsonArrayLength += chunk.length
                continue
            }
            const merged = new Uint8Array(pending.length + chunk.length)
            merged.set(pending)
            merged.set(chunk, pending.length)
            const { values, read } = Bun.JSONL.parseChunk(merged)
            pending = merged.subarray(read)
            for (const item of values) {
                batch.push(/** @type {Record<string, any>} */ (item))
                if (batch.length === Fylo.MAX_CPUS) {
                    await flush(batch)
                    batch = []
                    if (limit !== undefined && count >= limit) return count
                }
            }
        }
        if (isJsonArray) {
            const body = new Uint8Array(jsonArrayLength)
            let offset = 0
            for (const c of jsonArrayChunks) {
                body.set(c, offset)
                offset += c.length
            }
            let data
            try {
                data = JSON.parse(new TextDecoder().decode(body))
            } catch {
                throw new Error('Invalid JSON in import response')
            }
            const items = /** @type {Record<string, any>[]} */ (Array.isArray(data) ? data : [data])
            for (let i = 0; i < items.length; i += Fylo.MAX_CPUS) {
                if (limit !== undefined && count >= limit) break
                await flush(items.slice(i, i + Fylo.MAX_CPUS))
            }
        } else {
            if (pending.length > 0) {
                const { values } = Bun.JSONL.parseChunk(pending)
                for (const item of values) batch.push(/** @type {Record<string, any>} */ (item))
            }
            if (batch.length > 0) await flush(batch)
        }
        return count
    }
    /**
     * Gets an exported stream of documents from a collection.
     */
    /** @param {string} collection @returns {AsyncGenerator<Record<string, any>, void, unknown>} */
    static async *exportBulkData(collection) {
        yield* Fylo.defaultEngine.exportBulkData(collection)
    }
    /**
     * Gets a document from a collection.
     * @param {string} collection The name of the collection.
     * @param {TTIDValue} _id The ID of the document.
     * @param {boolean} onlyId Whether to only return the ID of the document.
     * @returns The document or the ID of the document.
     */
    static getDoc(collection, _id, onlyId = false) {
        validateDocId(_id)
        return Fylo.defaultEngine.getDoc(collection, _id, onlyId)
    }
    /**
     * Puts multiple documents into a collection.
     * @param {string} collection The name of the collection.
     * @param {Record<string, any>[]} batch The documents to put.
     * @returns The IDs of the documents.
     */
    async batchPutData(collection, batch) {
        await this.ready()
        const batches = []
        const ids = []
        if (batch.length > navigator.hardwareConcurrency) {
            for (let i = 0; i < batch.length; i += navigator.hardwareConcurrency) {
                batches.push(batch.slice(i, i + navigator.hardwareConcurrency))
            }
        } else batches.push(batch)
        for (const itemBatch of batches) {
            const writeResults = await Promise.allSettled(
                itemBatch.map((data) => this.putData(collection, data))
            )
            for (const _id of writeResults
                .filter((item) => item.status === 'fulfilled')
                .map((item) => item.value)) {
                ids.push(_id)
            }
        }
        return ids
    }
    /**
     * Puts a document into a collection.
     * @param collection The name of the collection.
     * @param data The document to put.
     * @returns The ID of the document.
     */
    /** @param {TTIDValue | undefined} existingId @returns {Promise<TTIDValue>} */
    static async uniqueTTID(existingId) {
        let _id
        const prev = Fylo.ttidLock
        Fylo.ttidLock = prev.then(async () => {
            _id = existingId ? TTID.generate(existingId) : TTID.generate()
        })
        await Fylo.ttidLock
        return /** @type {TTIDValue} */ (/** @type {unknown} */ (_id))
    }
    /**
     * Loads schema-driven encryption config for a collection and emits
     * `cipher.configured` if this call was the one that flipped the global
     * Cipher from unconfigured to configured.
     * @param {string} collection
     * @returns {Promise<void>}
     */
    async loadEncryptionWithEvent(collection) {
        const before = Cipher.isConfigured()
        await Fylo.loadEncryption(collection)
        if (!before && Cipher.isConfigured()) {
            emitFyloEvent(this.onEvent, { type: 'cipher.configured', collection })
        }
    }
    /** @param {string} collection @param {Record<string, any>} data @returns {Promise<{ _id: TTIDValue, doc: Record<string, any>, previousId?: TTIDValue }>} */
    async prepareInsert(collection, data) {
        await this.ready()
        await this.loadEncryptionWithEvent(collection)
        const currId = Object.keys(data).shift()
        const hasExistingId = typeof currId === 'string' && TTID.isTTID(currId)
        const _id = hasExistingId ? await Fylo.uniqueTTID(currId) : await Fylo.uniqueTTID(undefined)
        let doc = hasExistingId ? Object.values(data).shift() : data
        if (Fylo.STRICT) doc = await validateAgainstHead(collection, doc)
        return { _id, doc, previousId: hasExistingId ? currId : undefined }
    }
    /** @param {string} collection @param {TTIDValue} _id @param {Record<string, any>} doc @param {TTIDValue | undefined} previousId @returns {Promise<TTIDValue>} */
    async executePutDataDirect(collection, _id, doc, previousId) {
        await this.ready()
        if (previousId) await this.engine.replaceDocumentVersion(collection, previousId, _id, doc)
        else await this.engine.putDocument(collection, _id, doc)
        if (Fylo.LOGGING) console.log(`Finished Writing ${_id}`)
        return _id
    }
    /** @param {string} collection @param {Record<TTIDValue, Record<string, any>>} newDoc @param {Record<TTIDValue, Record<string, any>>} [oldDoc] @returns {Promise<TTIDValue>} */
    async executePatchDocDirect(collection, newDoc, oldDoc = {}) {
        await this.ready()
        await this.loadEncryptionWithEvent(collection)
        const _id = Object.keys(newDoc).shift()
        if (!_id) throw new Error('this document does not contain an TTID')
        validateDocId(_id)
        let existingDoc = oldDoc[_id]
        if (!existingDoc) {
            const existing = await this.engine.getDoc(collection, _id).once()
            existingDoc = existing[_id]
        }
        if (!existingDoc) return _id
        const currData = { ...existingDoc, ...newDoc[_id] }
        let docToWrite = currData
        const _newId = await Fylo.uniqueTTID(_id)
        if (Fylo.STRICT) docToWrite = await validateAgainstHead(collection, currData)
        const nextId = await this.engine.patchDocument(
            collection,
            _id,
            _newId,
            docToWrite,
            existingDoc
        )
        if (Fylo.LOGGING) console.log(`Finished Updating ${_id} to ${nextId}`)
        return nextId
    }
    /** @param {string} collection @param {TTIDValue} _id @returns {Promise<void>} */
    async executeDelDocDirect(collection, _id) {
        await this.ready()
        validateDocId(_id)
        await this.engine.deleteDocument(collection, _id)
        if (Fylo.LOGGING) console.log(`Finished Deleting ${_id}`)
    }
    /** @param {string} collection @param {Record<string, any>} data @returns {Promise<TTIDValue>} */
    async putData(collection, data) {
        const { _id, doc, previousId } = await this.prepareInsert(collection, data)
        await this.executePutDataDirect(collection, _id, doc, previousId)
        return _id
    }
    /**
     * Patches a document in a collection.
     * @param {string} collection The name of the collection.
     * @param {Record<TTIDValue, Record<string, any>>} newDoc The new document data.
     * @param {Record<TTIDValue, Record<string, any>>} oldDoc The old document data.
     * @returns The number of documents patched.
     */
    async patchDoc(collection, newDoc, oldDoc = {}) {
        return await this.executePatchDocDirect(collection, newDoc, oldDoc)
    }
    /**
     * Patches documents in a collection.
     * @param {string} collection The name of the collection.
     * @param {StoreUpdate} updateSchema The update schema.
     * @returns The number of documents patched.
     */
    async patchDocs(collection, updateSchema) {
        await this.loadEncryptionWithEvent(collection)
        let count = 0
        const promises = []
        for await (const value of this.findDocs(collection, updateSchema.$where ?? {}).collect()) {
            if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
                const [_id, current] = Object.entries(value)[0] ?? []
                if (_id && current) {
                    promises.push(
                        this.patchDoc(collection, { [_id]: updateSchema.$set }, { [_id]: current })
                    )
                    count++
                }
            }
        }
        await Promise.all(promises)
        return count
    }
    /**
     * Deletes a document from a collection.
     * @param collection The name of the collection.
     * @param _id The ID of the document.
     * @returns The number of documents deleted.
     */
    /** @param {string} collection @param {TTIDValue} _id @returns {Promise<void>} */
    async delDoc(collection, _id) {
        await this.executeDelDocDirect(collection, _id)
    }
    /**
     * Deletes documents from a collection.
     * @param collection The name of the collection.
     * @param deleteSchema The delete schema.
     * @returns The number of documents deleted.
     */
    /** @param {string} collection @param {StoreDelete} deleteSchema @returns {Promise<number>} */
    async delDocs(collection, deleteSchema) {
        await this.loadEncryptionWithEvent(collection)
        let count = 0
        const promises = []
        for await (const value of this.findDocs(collection, deleteSchema).collect()) {
            if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
                const _id = Object.keys(value).find((docId) => TTID.isTTID(docId))
                if (_id) {
                    promises.push(this.delDoc(collection, _id))
                    count++
                }
            }
        }
        await Promise.all(promises)
        return count
    }
    /**
     * Joins documents from two collections.
     * @param join The join schema.
     * @returns The joined documents.
     */
    /** @param {StoreJoin} join @returns {Promise<JoinDocsResult>} */
    static async joinDocs(join) {
        return await Fylo.defaultEngine.joinDocs(join)
    }
    /**
     * Finds documents in a collection.
     * @param collection The name of the collection.
     * @param query The query schema.
     * @returns The found documents.
     */
    /** @param {string} collection @param {StoreQuery} query @returns {FindDocsResult} */
    static findDocs(collection, query) {
        return Fylo.defaultEngine.findDocs(collection, query)
    }
}

/**
 * RLS-scoped FYLO facade. Every operation delegates to a backing `Fylo`
 * instance after authorizing the caller against collection rules.
 */
export class AuthenticatedFylo {
    /** @type {Fylo} */
    fylo
    /** @type {FyloAuthContext} */
    auth
    /**
     * @param {Fylo} fylo
     * @param {FyloAuthContext} auth
     */
    constructor(fylo, auth) {
        this.fylo = fylo
        this.auth = auth
    }
    /**
     * @param {{ action: FyloAuthAction, collection: string, docId?: string, data?: Record<string, any>, existing?: Record<string, any> }} args
     * @returns {Promise<void>}
     */
    async _authorize(args) {
        await authorizeOperation({
            collection: args.collection,
            schemaDir: schemaEnv(),
            auth: this.auth,
            action: args.action,
            docId: args.docId,
            data: args.data,
            existing: args.existing
        })
    }
    /**
     * @param {string} collection
     * @param {Record<string, any>} doc
     */
    async _isVisible(collection, doc) {
        return await isDocVisible({
            collection,
            schemaDir: schemaEnv(),
            auth: this.auth,
            doc
        })
    }
    /** @param {Record<string, any>} data @returns {TTIDValue | undefined} */
    firstDocId(data) {
        return Object.keys(data).find((key) => TTID.isTTID(key))
    }
    /** @param {string} collection @returns {Promise<void>} */
    async createCollection(collection) {
        await this._authorize({ action: 'collection:create', collection })
        return await this.fylo.createCollection(collection)
    }
    /** @param {string} collection @returns {Promise<void>} */
    async dropCollection(collection) {
        await this._authorize({ action: 'collection:drop', collection })
        return await this.fylo.dropCollection(collection)
    }
    /** @param {string} collection @returns {Promise<CollectionRebuildResult>} */
    async rebuildCollection(collection) {
        await this._authorize({ action: 'collection:rebuild', collection })
        return await this.fylo.rebuildCollection(collection)
    }
    /** @param {string} collection @returns {Promise<CollectionInspectResult>} */
    async inspectCollection(collection) {
        await this._authorize({ action: 'collection:inspect', collection })
        return await this.fylo.inspectCollection(collection)
    }
    /** @param {string} collection @param {TTIDValue} _id @param {boolean} [onlyId] @returns {GetDocResult} */
    getDoc(collection, _id, onlyId = false) {
        validateDocId(_id)
        const self = this
        const source = this.fylo.getDoc(collection, _id, onlyId)
        return /** @type {GetDocResult} */ ({
            async *[Symbol.asyncIterator]() {
                await self._authorize({ action: 'doc:read', collection, docId: _id })
                for await (const value of source) {
                    if (onlyId) {
                        yield value
                        continue
                    }
                    const envelope = /** @type {Record<string, Record<string, any>>} */ (value)
                    const [, doc] = Object.entries(envelope)[0] ?? [undefined, undefined]
                    if (!doc) continue
                    if (await self._isVisible(collection, doc)) yield envelope
                }
            },
            /** @returns {Promise<Record<TTIDValue, Record<string, any>>>} */
            async once() {
                await self._authorize({ action: 'doc:read', collection, docId: _id })
                const result = await source.once()
                if (Object.keys(result).length === 0) return result
                const doc = /** @type {Record<string, any>} */ (result[_id])
                if (!(await self._isVisible(collection, doc))) return {}
                return result
            },
            async *onDelete() {
                await self._authorize({ action: 'doc:read', collection, docId: _id })
                yield* source.onDelete()
            }
        })
    }
    /** @param {string} collection @param {TTIDValue} _id @param {boolean} [onlyId] @returns {Promise<Record<TTIDValue, Record<string, any>> | TTIDValue | null>} */
    async getLatest(collection, _id, onlyId = false) {
        await this._authorize({ action: 'doc:read', collection, docId: _id })
        if (onlyId) return await this.fylo.getLatest(collection, _id, true)
        const result = /** @type {Record<TTIDValue, Record<string, any>> | null} */ (
            await this.fylo.getLatest(collection, _id)
        )
        if (!result) return result
        const entries = Object.entries(result)
        if (entries.length === 0) return result
        const [, doc] = entries[0]
        if (!(await this._isVisible(collection, doc))) return {}
        return result
    }
    /** @param {string} collection @param {TTIDValue} _id @returns {Promise<FyloHistoryEntry[]>} */
    async getHistory(collection, _id) {
        await this._authorize({ action: 'doc:read', collection, docId: _id })
        const history = await this.fylo.getHistory(collection, _id)
        if (history.length === 0) return history
        // History is per-lineage; if the head version is invisible to the
        // user, the whole lineage is hidden. Otherwise return all entries.
        const headEntry = history.find((e) => e.isHead)
        if (headEntry && !(await this._isVisible(collection, headEntry.data))) return []
        return history
    }
    /** @param {string} collection @param {StoreQuery} query @returns {FindDocsResult} */
    findDocs(collection, query) {
        const self = this
        // Projection queries ($select / $onlyIds / $groupBy) yield flat rows
        // that have no envelope to evaluate read.filter against — would
        // silently bypass RLS. Refuse them on rules-protected collections;
        // the caller can query for full envelopes and project client-side.
        if (queryHasProjection(query)) {
            return /** @type {FindDocsResult} */ ({
                async *[Symbol.asyncIterator]() {
                    await self._assertNoProjectionWhenRlsEnabled(collection)
                    yield* self.fylo.findDocs(collection, query)
                },
                async *collect() {
                    await self._assertNoProjectionWhenRlsEnabled(collection)
                    yield* self.fylo.findDocs(collection, query).collect()
                },
                async *onDelete() {
                    await self._assertNoProjectionWhenRlsEnabled(collection)
                    yield* self.fylo.findDocs(collection, query).onDelete()
                }
            })
        }
        const source = this.fylo.findDocs(collection, query)
        return /** @type {FindDocsResult} */ ({
            async *[Symbol.asyncIterator]() {
                await self._authorize({ action: 'doc:find', collection })
                for await (const value of source) {
                    const filtered = await filterEnvelope(self, collection, value)
                    if (filtered !== undefined) yield filtered
                }
            },
            async *collect() {
                await self._authorize({ action: 'doc:find', collection })
                for await (const value of source.collect()) {
                    const filtered = await filterEnvelope(self, collection, value)
                    if (filtered !== undefined) yield filtered
                }
            },
            async *onDelete() {
                await self._authorize({ action: 'doc:find', collection })
                yield* source.onDelete()
            }
        })
    }
    /**
     * Throw if `collection` has a rules file and the caller is using a
     * projection query — RLS cannot be applied to flat projected rows.
     * @param {string} collection
     */
    async _assertNoProjectionWhenRlsEnabled(collection) {
        const rules = await loadRules(collection, schemaEnv())
        if (rules) {
            throw new Error(
                `RLS-protected collection '${collection}' cannot be queried with projections ` +
                    `($select / $onlyIds / $groupBy). Query for full envelopes and project client-side.`
            )
        }
        // No rules → allow the projection through; the caller's _authorize
        // would have caught this for collections that are RLS-protected.
        await this._authorize({ action: 'doc:find', collection })
    }
    /** @param {StoreJoin} join @returns {Promise<JoinDocsResult>} */
    async joinDocs(join) {
        // For joins, both collections must permit the action under the user's role.
        await this._authorize({
            action: 'join:execute',
            collection: String(join.$leftCollection)
        })
        await this._authorize({
            action: 'join:execute',
            collection: String(join.$rightCollection)
        })
        return await this.fylo.joinDocs(join)
    }
    /** @param {string} collection @returns {AsyncGenerator<Record<string, any>, void, unknown>} */
    async *exportBulkData(collection) {
        await this._authorize({ action: 'bulk:export', collection })
        for await (const doc of this.fylo.exportBulkData(collection)) {
            if (await this._isVisible(collection, doc)) yield doc
        }
    }
    /** @param {string} collection @param {URL} url @param {number | ImportBulkDataOptions} [limitOrOptions] @returns {Promise<number>} */
    async importBulkData(collection, url, limitOrOptions) {
        await this._authorize({ action: 'bulk:import', collection })
        if (typeof limitOrOptions === 'number')
            return await this.fylo.importBulkData(collection, url, limitOrOptions)
        return await this.fylo.importBulkData(collection, url, limitOrOptions)
    }
    /** @param {string} SQL @returns {ReturnType<Fylo['executeSQL']>} */
    async executeSQL(SQL) {
        // Re-dispatch the SQL through this scoped client so each branch is
        // authorized by the same rules as the equivalent direct call. This
        // avoids brittle string parsing and gives RLS proper per-op coverage.
        const operationMatch = SQL.match(/^(SELECT|INSERT|UPDATE|DELETE|CREATE|DROP)/i)
        const operation = operationMatch?.[0]?.toUpperCase()
        if (!operation) throw new Error('Missing SQL Operation')
        switch (operation) {
            case 'CREATE':
                return await this.createCollection(
                    /** @type {{ $collection: string }} */ (Parser.parse(SQL)).$collection
                )
            case 'DROP':
                return await this.dropCollection(
                    /** @type {{ $collection: string }} */ (Parser.parse(SQL)).$collection
                )
            case 'SELECT': {
                const query = /** @type {StoreQuery} */ (Parser.parse(SQL))
                if (SQL.includes('JOIN'))
                    return await this.joinDocs(/** @type {StoreJoin} */ (query))
                const selectedCollection = query.$collection
                delete query.$collection
                /** @type {TTIDValue[] | Record<string, any>} */
                let docs = query.$onlyIds ? [] : {}
                for await (const data of this.findDocs(
                    String(selectedCollection),
                    query
                ).collect()) {
                    if (typeof data === 'object')
                        docs = /** @type {{ appendGroup(target: any, value: any): any }} */ (
                            /** @type {unknown} */ (Object)
                        ).appendGroup(docs, data)
                    else docs.push(data)
                }
                return docs
            }
            case 'INSERT': {
                const insert = /** @type {StoreInsert} */ (Parser.parse(SQL))
                const insertCollection = insert.$collection
                delete insert.$collection
                return await this.putData(String(insertCollection), insert.$values)
            }
            case 'UPDATE': {
                const update = /** @type {StoreUpdate} */ (Parser.parse(SQL))
                const updateCollection = update.$collection
                delete update.$collection
                return await this.patchDocs(String(updateCollection), update)
            }
            case 'DELETE': {
                const del = /** @type {StoreDelete} */ (Parser.parse(SQL))
                const deleteCollection = del.$collection
                delete del.$collection
                return await this.delDocs(String(deleteCollection), del)
            }
            default:
                throw new Error('Invalid Operation')
        }
    }
    /** @param {string} collection @param {Record<string, any>[]} batch @returns {Promise<TTIDValue[]>} */
    async batchPutData(collection, batch) {
        for (const data of batch) {
            await this._authorize({ action: 'doc:create', collection, data })
        }
        return await this.fylo.batchPutData(collection, batch)
    }
    /** @param {string} collection @param {Record<string, any>} data @returns {Promise<TTIDValue>} */
    async putData(collection, data) {
        await this._authorize({
            action: 'doc:create',
            collection,
            docId: this.firstDocId(data),
            data
        })
        return await this.fylo.putData(collection, data)
    }
    /** @param {string} collection @param {Record<TTIDValue, Record<string, any>>} newDoc @param {Record<TTIDValue, Record<string, any>>} [oldDoc] @returns {Promise<TTIDValue>} */
    async patchDoc(collection, newDoc, oldDoc = {}) {
        const _id = this.firstDocId(newDoc)
        if (!_id) throw new Error('patchDoc: newDoc must contain a TTID-keyed entry')
        let existing = oldDoc[_id]
        if (!existing) {
            const fetched = await this.fylo.engine.getDoc(collection, _id).once()
            existing = fetched[_id]
        }
        if (!existing)
            throw new FyloAuthError({
                auth: this.auth,
                action: 'doc:update',
                collection,
                docId: _id
            })
        await this._authorize({
            action: 'doc:update',
            collection,
            docId: _id,
            data: newDoc[_id],
            existing
        })
        return await this.fylo.patchDoc(collection, newDoc, oldDoc)
    }
    /** @param {string} collection @param {StoreUpdate} updateSchema @returns {Promise<number>} */
    async patchDocs(collection, updateSchema) {
        // Iterate via the scoped findDocs so the role's read.filter applies —
        // bulk update operates only on the visible subset, matching the Atlas
        // model where update is gated by the read scope.
        // Per-doc FyloAuthError is treated as "filter doesn't match" and
        // silently skipped (SQL UPDATE-style — non-matching rows are no-ops).
        let count = 0
        const promises = []
        for await (const value of this.findDocs(collection, updateSchema.$where ?? {}).collect()) {
            if (typeof value !== 'object' || value === null || Array.isArray(value)) continue
            const entry = Object.entries(value)[0]
            if (!entry) continue
            const [_id, existing] = entry
            try {
                await this._authorize({
                    action: 'doc:update',
                    collection,
                    docId: _id,
                    data: updateSchema.$set,
                    existing: /** @type {Record<string, any>} */ (existing)
                })
            } catch (err) {
                if (err instanceof FyloAuthError) continue
                throw err
            }
            promises.push(
                this.fylo.patchDoc(
                    collection,
                    { [_id]: updateSchema.$set },
                    { [_id]: /** @type {Record<string, any>} */ (existing) }
                )
            )
            count++
        }
        await Promise.all(promises)
        return count
    }
    /** @param {string} collection @param {TTIDValue} _id @returns {Promise<void>} */
    async delDoc(collection, _id) {
        const fetched = await this.fylo.engine.getDoc(collection, _id).once()
        const existing = fetched[_id]
        if (!existing)
            throw new FyloAuthError({
                auth: this.auth,
                action: 'doc:delete',
                collection,
                docId: _id
            })
        await this._authorize({
            action: 'doc:delete',
            collection,
            docId: _id,
            existing: /** @type {Record<string, any>} */ (existing)
        })
        return await this.fylo.delDoc(collection, _id)
    }
    /** @param {string} collection @param {StoreDelete} deleteSchema @returns {Promise<number>} */
    async delDocs(collection, deleteSchema) {
        // Same model as patchDocs: scoped iteration + per-doc auth, with
        // FyloAuthError treated as a filter miss.
        let count = 0
        const promises = []
        for await (const value of this.findDocs(collection, deleteSchema).collect()) {
            if (typeof value !== 'object' || value === null || Array.isArray(value)) continue
            const entry = Object.entries(value)[0]
            if (!entry) continue
            const [_id, existing] = entry
            try {
                await this._authorize({
                    action: 'doc:delete',
                    collection,
                    docId: _id,
                    existing: /** @type {Record<string, any>} */ (existing)
                })
            } catch (err) {
                if (err instanceof FyloAuthError) continue
                throw err
            }
            promises.push(this.fylo.delDoc(collection, _id))
            count++
        }
        await Promise.all(promises)
        return count
    }
}

/**
 * Whether a query asks for a projection (flat rows or grouped maps) instead
 * of full doc envelopes. Returned by the inner queryEngine when any of these
 * keys is set: `$select`, `$onlyIds`, `$groupBy`.
 * @param {Record<string, any> | undefined} query
 * @returns {boolean}
 */
function queryHasProjection(query) {
    if (!query) return false
    return Boolean(query.$select || query.$onlyIds || query.$groupBy)
}

/**
 * Drop a `findDocs`-style envelope if the user can't see the underlying doc.
 * Returns the envelope unchanged when visible, or `undefined` to skip.
 *
 * KNOWN LIMITATION: queries that project ($select, $onlyIds, $groupBy) yield
 * flat rows or grouped maps without the originating doc, so RLS read.filter
 * cannot be evaluated and these shapes flow through unchecked. Treat
 * projection queries as read-all for the matched set; if you need RLS on
 * projections, query for full envelopes and project client-side.
 *
 * @param {AuthenticatedFylo} self
 * @param {string} collection
 * @param {unknown} value
 * @returns {Promise<unknown>}
 */
async function filterEnvelope(self, collection, value) {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) {
        return value
    }
    const entries = Object.entries(/** @type {Record<string, any>} */ (value))
    if (entries.length !== 1) return value
    const [, doc] = entries[0]
    if (typeof doc !== 'object' || doc === null) return value
    return (await self._isVisible(collection, /** @type {Record<string, any>} */ (doc)))
        ? value
        : undefined
}
