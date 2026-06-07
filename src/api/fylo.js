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
import { createQueryCache } from '../cache/query.js'
import { VersionRepository } from '../versioning/repository.js'
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
 * @typedef {import('../cache/query.js').QueryCache} QueryCache
 * @typedef {import('../types/fylo.js').GetDocResult<Record<string, any>>} GetDocResult
 * @typedef {import('../types/fylo.js').FindDocsResult<Record<string, any>>} FindDocsResult
 * @typedef {import('../types/fylo.js').DeletedDocsResult<Record<string, any>>} DeletedDocsResult
 * @typedef {import('../types/fylo.js').JoinDocsResult<Record<string, any>, Record<string, any>>} JoinDocsResult
 */

/**
 * @typedef {import('../security/import-guard.js').ImportBulkDataOptions} ImportBulkDataOptions
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
    /** @type {QueryCache | undefined} */
    cache
    /** @type {Promise<void>} */
    startup
    /** @type {(strings: TemplateStringsArray, ...values: unknown[]) => Promise<unknown>} */
    sql
    /**
     * @param {string} root
     * @param {FyloOptions} [options]
     */
    constructor(root, options = {}) {
        root = Fylo.rootFromPath(root)
        if (options.versioning?.resolve !== false) {
            root = VersionRepository.resolveActiveRoot(root)
        }
        if (Object.hasOwn(options, 'root')) {
            throw new Error(
                'Fylo constructor config must not include root; pass the database path as the first argument'
            )
        }
        syncChexSchemaEnv()
        this.rlsEnabled = options.rls === true
        this.onEvent = options.onEvent
        this.queue = options.queue ? new LocalQueue({ root }) : undefined
        this.cache = createQueryCache(root, options.cache)
        this.engine = new FilesystemEngine(root, {
            sync: options.sync,
            syncMode: options.syncMode,
            worm: options.worm,
            index: options.index,
            onEvent: options.onEvent,
            queue: this.queue,
            queryCache: this.cache
        })
        this.sql = this.createSqlTag()
        this.startup = this.bootstrapCollectionsFromSchemas()
        return this.createFyloProxy()
    }

    /** @returns {Promise<void>} */
    async ready() {
        await this.startup
    }

    async close() {
        await this.cache?.close?.()
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
    /**
     * @param {string} root
     * @returns {string}
     */
    static rootFromPath(root) {
        if (typeof root !== 'string' || root.length === 0) {
            throw new Error('Fylo constructor requires a database path string')
        }
        if (root.startsWith('fylo://')) {
            throw new Error('Fylo constructor accepts a filesystem path directly; remove fylo://')
        }
        return root
    }
    /** @returns {FilesystemEngine} */
    static get defaultEngine() {
        return new FilesystemEngine(Fylo.defaultRoot())
    }
    /**
     * @param {unknown} value
     * @returns {string}
     */
    static sqlValue(value) {
        if (value === null || value === undefined) return 'NULL'
        if (typeof value === 'number') {
            if (!Number.isFinite(value)) throw new Error('SQL parameter must be a finite number')
            return String(value)
        }
        if (typeof value === 'boolean') return value ? 'true' : 'false'
        if (typeof value === 'bigint') return value.toString()
        if (value instanceof Date) return `'${value.toISOString().replaceAll("'", "''")}'`
        if (typeof value === 'object') {
            throw new Error('SQL parameters must be scalar values')
        }
        return `'${String(value).replaceAll("'", "''")}'`
    }
    /** @returns {(strings: TemplateStringsArray, ...values: unknown[]) => Promise<unknown>} */
    /**
     * Internal: execute a raw SQL string. Prefer the `fylo.sql`...`` template tag for application code.
     * @param {string} SQL
     * @returns {Promise<unknown>}
     */
    async _sql(SQL) {
        await this.ready()
        const operationMatch = SQL.match(/^(SELECT|INSERT|UPDATE|DELETE|CREATE|DROP)/i)
        const operation = operationMatch?.[0]?.toUpperCase()
        if (!operation) throw new Error('Missing SQL Operation')
        const col = /** @type {{ $collection: string }} */ (Parser.parse(SQL)).$collection
        switch (operation) {
            case 'CREATE':
                return await new CollectionFacade(this, col).create()
            case 'DROP':
                return await new CollectionFacade(this, col).drop()
            case 'SELECT': {
                const query = /** @type {StoreQuery} */ (Parser.parse(SQL))
                if (SQL.includes('JOIN'))
                    return await this.joinDocs(/** @type {StoreJoin} */ (query))
                const selectedCollection = query.$collection
                delete query.$collection
                /** @type {TTIDValue[] | Record<string, any>} */
                let docs = query.$onlyIds ? [] : {}
                for await (const data of new CollectionFacade(this, String(selectedCollection))
                    .find(query)
                    .collect()) {
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
                return await new CollectionFacade(this, String(insertCollection)).put(
                    insert.$values
                )
            }
            case 'UPDATE': {
                const update = /** @type {StoreUpdate} */ (Parser.parse(SQL))
                const updateCol = update.$collection
                delete update.$collection
                return await new CollectionFacade(this, String(updateCol)).patchMany(update)
            }
            case 'DELETE': {
                const del = /** @type {StoreDelete} */ (Parser.parse(SQL))
                const deleteCollection = del.$collection
                delete del.$collection
                return await new CollectionFacade(this, String(deleteCollection)).deleteMany(del)
            }
            default:
                throw new Error('Invalid Operation')
        }
    }
    createSqlTag() {
        const fylo = this
        return /** @type {(strings: TemplateStringsArray, ...values: unknown[]) => Promise<unknown>} */ (
            async (strings, ...values) => {
                let statement = strings[0] ?? ''
                for (let index = 0; index < values.length; index++) {
                    statement += Fylo.sqlValue(values[index]) + (strings[index + 1] ?? '')
                }
                return await fylo._sql(statement)
            }
        )
    }
    /** @returns {Fylo} */
    createFyloProxy() {
        const fylo = this
        const reserved = new Set([
            'then',
            'constructor',
            'prototype',
            '_sql',
            ...Object.getOwnPropertyNames(Object.prototype),
            ...Object.getOwnPropertyNames(Fylo.prototype),
            ...Object.getOwnPropertyNames(fylo)
        ])
        return new Proxy(this, {
            get(target, prop, receiver) {
                if (typeof prop === 'symbol') return Reflect.get(target, prop, receiver)
                if (reserved.has(prop)) return Reflect.get(target, prop, receiver)
                return new CollectionFacade(fylo, prop)
            }
        })
    }
    /** @deprecated Use fylo.sql('...') or fylo.sql`...` instead */
    async executeSQL() {
        throw new Error(
            "fylo.executeSQL() has been removed. Use fylo.sql('SELECT ...') or the template tag fylo.sql`...` instead."
        )
    }
    // ── Static migration stubs ──────────────────────────────────────────────────
    /** @deprecated Construct a Fylo instance and use fylo[collection].create() */
    /** @param {string} collection */
    static async createCollection(collection) {
        throw new Error(
            `Fylo.createCollection('${collection}') has been removed. Construct a Fylo instance and use fylo['${collection}'].create().`
        )
    }
    /** @deprecated Construct a Fylo instance and use fylo[collection].drop() */
    /** @param {string} collection */
    static async dropCollection(collection) {
        throw new Error(
            `Fylo.dropCollection('${collection}') has been removed. Construct a Fylo instance and use fylo['${collection}'].drop().`
        )
    }
    /** @deprecated Construct a Fylo instance and use fylo[collection].rebuild() */
    /** @param {string} collection */
    static async rebuildCollection(collection) {
        throw new Error(
            `Fylo.rebuildCollection('${collection}') has been removed. Construct a Fylo instance and use fylo['${collection}'].rebuild().`
        )
    }
    /** @deprecated Construct a Fylo instance and use fylo[collection].inspect() */
    /** @param {string} collection */
    static async inspectCollection(collection) {
        throw new Error(
            `Fylo.inspectCollection('${collection}') has been removed. Construct a Fylo instance and use fylo['${collection}'].inspect().`
        )
    }
    // ── Migration stubs (old method-first API removed) ──────────────────────────
    /** @deprecated Use fylo[collection].create() instead */
    /** @param {string} collection */
    async createCollection(collection) {
        throw new Error(
            `fylo.createCollection('${collection}') has been removed. Use fylo['${collection}'].create() instead.`
        )
    }
    /** @deprecated Use fylo[collection].drop() instead */
    /** @param {string} collection */
    async dropCollection(collection) {
        throw new Error(
            `fylo.dropCollection('${collection}') has been removed. Use fylo['${collection}'].drop() instead.`
        )
    }
    /** @deprecated Use fylo[collection].rebuild() instead */
    /** @param {string} collection */
    async rebuildCollection(collection) {
        throw new Error(
            `fylo.rebuildCollection('${collection}') has been removed. Use fylo['${collection}'].rebuild() instead.`
        )
    }
    /** @deprecated Use fylo[collection].inspect() instead */
    /** @param {string} collection */
    async inspectCollection(collection) {
        throw new Error(
            `fylo.inspectCollection('${collection}') has been removed. Use fylo['${collection}'].inspect() instead.`
        )
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
    /** @deprecated Use fylo[collection].get(id) instead */
    /** @param {string} collection @param {TTIDValue} [_id] */
    getDoc(collection, _id) {
        throw new Error(
            `fylo.getDoc('${collection}', id) has been removed. Use fylo['${collection}'].get(id) instead.`
        )
    }
    /** @deprecated Use fylo[collection].latest(id) instead */
    /** @param {string} collection @param {TTIDValue} [_id] */
    async getLatest(collection, _id) {
        throw new Error(
            `fylo.getLatest('${collection}', id) has been removed. Use fylo['${collection}'].latest(id) instead.`
        )
    }
    /** @deprecated Use fylo[collection].find(query) instead */
    /** @param {string} collection */
    findDocs(collection) {
        throw new Error(
            `fylo.findDocs('${collection}', query) has been removed. Use fylo['${collection}'].find(query) instead.`
        )
    }
    /** @deprecated Use fylo[collection].findDeleted(query) instead */
    /** @param {string} collection */
    findDeletedDocs(collection) {
        throw new Error(
            `fylo.findDeletedDocs('${collection}', query) has been removed. Use fylo['${collection}'].findDeleted(query) instead.`
        )
    }
    /** @param {StoreJoin} join @returns {Promise<JoinDocsResult>} */
    async joinDocs(join) {
        await this.ready()
        return await this.engine.joinDocs(join)
    }
    /** @deprecated Use fylo[collection].export() instead */
    /** @param {string} collection */
    async *exportBulkData(collection) {
        throw new Error(
            `fylo.exportBulkData('${collection}') has been removed. Use fylo['${collection}'].export() instead.`
        )
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
            await new CollectionFacade(this, collection).batchPut(items)
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
    /** @deprecated Construct a Fylo instance and use fylo[collection].export() */
    /** @param {string} collection */
    static async */** @param {string} collection */
    exportBulkData(collection) {
        throw new Error(
            `Fylo.exportBulkData('${collection}') has been removed. Construct a Fylo instance and use fylo['${collection}'].export().`
        )
    }
    /** @deprecated Construct a Fylo instance and use fylo[collection].get(id) */
    /** @param {string} collection */
    static getDoc(collection) {
        throw new Error(
            `Fylo.getDoc('${collection}', id) has been removed. Construct a Fylo instance and use fylo['${collection}'].get(id).`
        )
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
        const _id = hasExistingId ? currId : await Fylo.uniqueTTID(undefined)
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
        if (Fylo.STRICT) docToWrite = await validateAgainstHead(collection, currData)
        const nextId = await this.engine.patchDocument(
            collection,
            _id,
            _id,
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
    /** @deprecated Use fylo[collection].put(data) instead */
    /** @param {string} collection */
    async putData(collection) {
        throw new Error(
            `fylo.putData('${collection}', data) has been removed. Use fylo['${collection}'].put(data) instead.`
        )
    }
    /** @deprecated Use fylo[collection].patch(id, patch) instead */
    /** @param {string} collection */
    async patchDoc(collection) {
        throw new Error(
            `fylo.patchDoc('${collection}', ...) has been removed. Use fylo['${collection}'].patch(id, patch) instead.`
        )
    }
    /** @deprecated Use fylo[collection].patchMany(update) instead */
    /** @param {string} collection */
    async patchDocs(collection) {
        throw new Error(
            `fylo.patchDocs('${collection}', update) has been removed. Use fylo['${collection}'].patchMany(update) instead.`
        )
    }
    /** @deprecated Use fylo[collection].delete(id) instead */
    /** @param {string} collection */
    async delDoc(collection) {
        throw new Error(
            `fylo.delDoc('${collection}', id) has been removed. Use fylo['${collection}'].delete(id) instead.`
        )
    }
    /** @deprecated Use fylo[collection].deleteMany(query) instead */
    /** @param {string} collection */
    async delDocs(collection) {
        throw new Error(
            `fylo.delDocs('${collection}', query) has been removed. Use fylo['${collection}'].deleteMany(query) instead.`
        )
    }
    /** @deprecated Use fylo[collection].restore(id) instead */
    /** @param {string} collection */
    async restoreDoc(collection) {
        throw new Error(
            `fylo.restoreDoc('${collection}', id) has been removed. Use fylo['${collection}'].restore(id) instead.`
        )
    }
    /** @deprecated Use fylo[collection].batchPut(batch) instead */
    /** @param {string} collection */
    async batchPutData(collection) {
        throw new Error(
            `fylo.batchPutData('${collection}', batch) has been removed. Use fylo['${collection}'].batchPut(batch) instead.`
        )
    }
    /** @deprecated Construct a Fylo instance and use fylo.joinDocs(join) */
    /** @param {StoreJoin} join */
    static async joinDocs(join) {
        return await Fylo.defaultEngine.joinDocs(join)
    }
    /** @deprecated Construct a Fylo instance and use fylo[collection].find(query) */
    /** @param {string} collection */
    /** @param {string} collection */
    static findDocs(collection) {
        throw new Error(
            `Fylo.findDocs('${collection}', query) has been removed. Construct a Fylo instance and use fylo['${collection}'].find(query).`
        )
    }
}

/**
 * Collection-scoped facade returned by `fylo.<collection>`.
 * Each access creates a new facade that binds the collection name
 * so callers write `fylo.users.get(id)` instead of `fylo.getDoc('users', id)`.
 */
export class CollectionFacade {
    /** @type {Fylo} */
    fylo
    /** @type {string} */
    collection
    /**
     * @param {Fylo} fylo
     * @param {string} collection
     */
    constructor(fylo, collection) {
        this.fylo = fylo
        this.collection = collection
    }
    /** @param {TTIDValue} id @param {boolean} [onlyId] */
    get(id, onlyId = false) {
        validateDocId(id)
        const source = this.fylo
            .ready()
            .then(() => this.fylo.engine.getDoc(this.collection, id, onlyId))
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
    /** @param {TTIDValue} id @param {boolean} [onlyId] */
    async latest(id, onlyId = false) {
        await this.fylo.ready()
        validateDocId(id)
        if (onlyId) return await this.fylo.engine.getLatest(this.collection, id, true)
        return await this.fylo.engine.getLatest(this.collection, id)
    }
    /** @param {StoreQuery} [query] */
    find(query = {}) {
        const source = this.fylo
            .ready()
            .then(() => this.fylo.engine.findDocs(this.collection, query))
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
    /** @param {StoreQuery} [query] */
    findDeleted(query = {}) {
        const source = this.fylo
            .ready()
            .then(() => this.fylo.engine.findDeletedDocs(this.collection, query))
        return {
            async *[Symbol.asyncIterator]() {
                yield* await source
            },
            async *collect() {
                yield* (await source).collect()
            }
        }
    }
    /** @param {Record<string, any>} data */
    async put(data) {
        const { _id, doc, previousId } = await this.fylo.prepareInsert(this.collection, data)
        return await this.fylo.executePutDataDirect(this.collection, _id, doc, previousId)
    }
    /** @param {Record<string, any>[]} batch */
    async batchPut(batch) {
        await this.fylo.ready()
        const ids = []
        const chunkSize = navigator.hardwareConcurrency
        for (let i = 0; i < batch.length; i += chunkSize) {
            const chunk = batch.slice(i, i + chunkSize)
            const results = await Promise.allSettled(chunk.map((data) => this.put(data)))
            for (const r of results) {
                if (r.status === 'fulfilled') ids.push(r.value)
            }
        }
        return ids
    }
    /** @param {TTIDValue} id @param {Record<string, any>} patch @param {Record<string, any>} [oldDoc] */
    async patch(id, patch, oldDoc = {}) {
        return await this.fylo.executePatchDocDirect(this.collection, { [id]: patch }, oldDoc)
    }
    /** @param {StoreUpdate} update */
    async patchMany(update) {
        await this.fylo.loadEncryptionWithEvent(this.collection)
        let count = 0
        for await (const value of this.find(update.$where ?? {}).collect()) {
            const entries = Object.entries(value)
            if (entries.length === 0) continue
            const [docId, existing] = entries[0]
            try {
                await this.fylo.executePatchDocDirect(
                    this.collection,
                    { [docId]: update.$set ?? existing },
                    { [docId]: /** @type {Record<string, any>} */ (existing) }
                )
                count++
            } catch (err) {
                if (err instanceof FyloAuthError) continue
                throw err
            }
        }
        return count
    }
    /** @param {TTIDValue} id */
    async ['delete'](id) {
        return await this.fylo.executeDelDocDirect(this.collection, id)
    }
    /** @param {StoreDelete} query */
    async deleteMany(query) {
        await this.fylo.loadEncryptionWithEvent(this.collection)
        let count = 0
        for await (const value of this.find(query).collect()) {
            const entries = Object.entries(value)
            if (entries.length === 0) continue
            const [docId] = entries[0]
            try {
                await this.fylo.executeDelDocDirect(this.collection, docId)
                count++
            } catch (err) {
                if (err instanceof FyloAuthError) continue
                throw err
            }
        }
        return count
    }
    /** @param {TTIDValue} id */
    async restore(id) {
        await this.fylo.ready()
        return await this.fylo.engine.restoreDocument(this.collection, id)
    }
    /** @returns {AsyncGenerator<Record<string, any>, void, unknown>} */
    async *export() {
        await this.fylo.ready()
        yield* this.fylo.engine.exportBulkData(this.collection)
    }
    /** @param {URL} url @param {number | ImportBulkDataOptions} [limitOrOptions] */
    async import(url, limitOrOptions) {
        return await this.fylo.importBulkData(this.collection, url, limitOrOptions)
    }
    async inspect() {
        await this.fylo.ready()
        return await this.fylo.engine.inspectCollection(this.collection)
    }
    async rebuild() {
        await this.fylo.ready()
        return await this.fylo.engine.rebuildCollection(this.collection)
    }
    async create() {
        await this.fylo.ready()
        return await this.fylo.engine.createCollection(this.collection)
    }
    async drop() {
        await this.fylo.ready()
        return await this.fylo.engine.dropCollection(this.collection)
    }
}

/**
 * RLS-scoped FYLO facade. Every operation delegates to a backing `Fylo`
 * instance after authorizing the caller against collection rules.
 */
/**
 * RLS-scoped FYLO facade with collection-first access.
 * `fylo.as(auth)` returns a Proxy that authorizes every operation
 * against collection rules. Access collections as `scoped.users.get(id)`.
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
        const reserved = new Set([
            'then',
            'constructor',
            'prototype',
            ...Object.getOwnPropertyNames(Object.prototype),
            ...Object.getOwnPropertyNames(AuthenticatedFylo.prototype),
            ...Object.getOwnPropertyNames(this)
        ])
        return new Proxy(this, {
            get(target, prop, receiver) {
                if (typeof prop === 'symbol') return Reflect.get(target, prop, receiver)
                if (reserved.has(prop)) return Reflect.get(target, prop, receiver)
                return new AuthenticatedCollectionFacade(target, prop)
            }
        })
    }
    /** @param {FyloAuthContext} auth @returns {AuthenticatedFylo} */
    as(auth) {
        return new AuthenticatedFylo(this.fylo, auth)
    }
    /** @param {StoreJoin} join @returns {Promise<JoinDocsResult>} */
    async joinDocs(join) {
        await this._authorize({ action: 'join:execute', collection: String(join.$leftCollection) })
        await this._authorize({ action: 'join:execute', collection: String(join.$rightCollection) })
        return await this.fylo.engine.joinDocs(join)
    }
    /** @deprecated Use scoped.sql`...` template tag instead */
    async executeSQL() {
        throw new Error('executeSQL() has been removed. Use the .sql`...` template tag instead.')
    }
    /**
     * Internal: execute a raw SQL string through RLS-scoped collection access.
     * @param {string} SQL
     */
    async _sql(SQL) {
        const operationMatch = SQL.match(/^(SELECT|INSERT|UPDATE|DELETE|CREATE|DROP)/i)
        const operation = operationMatch?.[0]?.toUpperCase()
        if (!operation) throw new Error('Missing SQL Operation')
        const col = /** @type {{ $collection: string }} */ (Parser.parse(SQL)).$collection
        switch (operation) {
            case 'CREATE':
                return await new AuthenticatedCollectionFacade(this, col).create()
            case 'DROP':
                return await new AuthenticatedCollectionFacade(this, col).drop()
            case 'SELECT': {
                const query = /** @type {StoreQuery} */ (Parser.parse(SQL))
                if (SQL.includes('JOIN'))
                    return await this.joinDocs(/** @type {StoreJoin} */ (query))
                const selectedCollection = query.$collection
                delete query.$collection
                /** @type {TTIDValue[] | Record<string, any>} */
                let docs = query.$onlyIds ? [] : {}
                for await (const data of new AuthenticatedCollectionFacade(
                    this,
                    String(selectedCollection)
                )
                    .find(query)
                    .collect()) {
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
                return await new AuthenticatedCollectionFacade(this, String(insertCollection)).put(
                    insert.$values
                )
            }
            case 'UPDATE': {
                const update = /** @type {StoreUpdate} */ (Parser.parse(SQL))
                const updateCol = update.$collection
                delete update.$collection
                return await new AuthenticatedCollectionFacade(this, String(updateCol)).patchMany(
                    update
                )
            }
            case 'DELETE': {
                const del = /** @type {StoreDelete} */ (Parser.parse(SQL))
                const deleteCollection = del.$collection
                delete del.$collection
                return await new AuthenticatedCollectionFacade(
                    this,
                    String(deleteCollection)
                ).deleteMany(del)
            }
            default:
                throw new Error('Invalid Operation')
        }
    }
    /** @type {Fylo['sql']} */
    get sql() {
        const self = this
        return async (strings, ...values) => {
            let statement = strings[0] ?? ''
            for (let index = 0; index < values.length; index++) {
                statement += Fylo.sqlValue(values[index]) + (strings[index + 1] ?? '')
            }
            return await self._sql(statement)
        }
    }
    // ── Internal helpers ────────────────────────────────────────────────────────
    /** @param {{ action: FyloAuthAction, collection: string, docId?: string, data?: Record<string, any>, existing?: Record<string, any> }} args */
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
    /** @param {string} collection @param {Record<string, any>} doc */
    async _isVisible(collection, doc) {
        return await isDocVisible({ collection, schemaDir: schemaEnv(), auth: this.auth, doc })
    }
    /** @param {Record<string, any>} data @returns {TTIDValue | undefined} */
    firstDocId(data) {
        return Object.keys(data).find((key) => TTID.isTTID(key))
    }
    /** @param {string} collection */
    async _assertNoProjectionWhenRlsEnabled(collection) {
        const rules = await loadRules(collection, schemaEnv())
        if (rules)
            throw new Error(
                `RLS-protected collection '${collection}' cannot be queried with projections ($select / $onlyIds / $groupBy). Query for full envelopes and project client-side.`
            )
        await this._authorize({ action: 'doc:find', collection })
    }
}

/**
 * RLS-wrapped collection facade. Each method authorizes the operation
 * against the scoped auth context, then delegates to the underlying Fylo engine.
 */
class AuthenticatedCollectionFacade {
    /** @type {AuthenticatedFylo} */
    authFylo
    /** @type {string} */
    collection
    /** @param {AuthenticatedFylo} authFylo @param {string} collection */
    constructor(authFylo, collection) {
        this.authFylo = authFylo
        this.collection = collection
    }
    async create() {
        await this.authFylo._authorize({ action: 'collection:create', collection: this.collection })
        return await this.authFylo.fylo.engine.createCollection(this.collection)
    }
    async drop() {
        await this.authFylo._authorize({ action: 'collection:drop', collection: this.collection })
        return await this.authFylo.fylo.engine.dropCollection(this.collection)
    }
    async rebuild() {
        await this.authFylo._authorize({
            action: 'collection:rebuild',
            collection: this.collection
        })
        return await this.authFylo.fylo.engine.rebuildCollection(this.collection)
    }
    async inspect() {
        await this.authFylo._authorize({
            action: 'collection:inspect',
            collection: this.collection
        })
        return await this.authFylo.fylo.engine.inspectCollection(this.collection)
    }
    /** @param {TTIDValue} id @param {boolean} [onlyId] */
    get(id, onlyId = false) {
        validateDocId(id)
        const self = this
        const source = this.authFylo.fylo.engine.getDoc(this.collection, id, onlyId)
        return {
            async *[Symbol.asyncIterator]() {
                await self.authFylo._authorize({
                    action: 'doc:read',
                    collection: self.collection,
                    docId: id
                })
                for await (const value of source) {
                    if (onlyId) {
                        yield value
                        continue
                    }
                    const envelope = /** @type {Record<string, Record<string, any>>} */ (value)
                    const [, doc] = Object.entries(envelope)[0] ?? [undefined, undefined]
                    if (!doc) continue
                    if (await self.authFylo._isVisible(self.collection, doc)) yield envelope
                }
            },
            async once() {
                await self.authFylo._authorize({
                    action: 'doc:read',
                    collection: self.collection,
                    docId: id
                })
                const result = await source.once()
                if (Object.keys(result).length === 0) return result
                const doc = /** @type {Record<string, any>} */ (result[id])
                if (!(await self.authFylo._isVisible(self.collection, doc))) return {}
                return result
            },
            async *onDelete() {
                await self.authFylo._authorize({
                    action: 'doc:read',
                    collection: self.collection,
                    docId: id
                })
                yield* source.onDelete()
            }
        }
    }
    /** @param {TTIDValue} id @param {boolean} [onlyId] */
    async latest(id, onlyId = false) {
        await this.authFylo._authorize({
            action: 'doc:read',
            collection: this.collection,
            docId: id
        })
        if (onlyId) return await this.authFylo.fylo.engine.getLatest(this.collection, id, true)
        const result = /** @type {Record<TTIDValue, Record<string, any>> | null} */ (
            await this.authFylo.fylo.engine.getLatest(this.collection, id)
        )
        if (!result) return result
        const entries = Object.entries(result)
        if (entries.length === 0) return result
        const [, doc] = entries[0]
        if (!(await this.authFylo._isVisible(this.collection, doc))) return {}
        return result
    }
    find(query = {}) {
        const self = this
        if (queryHasProjection(query)) {
            return {
                async *[Symbol.asyncIterator]() {
                    await self.authFylo._assertNoProjectionWhenRlsEnabled(self.collection)
                    yield* self.authFylo.fylo.engine.findDocs(self.collection, query)
                },
                async *collect() {
                    await self.authFylo._assertNoProjectionWhenRlsEnabled(self.collection)
                    yield* self.authFylo.fylo.engine.findDocs(self.collection, query).collect()
                },
                async *onDelete() {
                    await self.authFylo._assertNoProjectionWhenRlsEnabled(self.collection)
                    yield* self.authFylo.fylo.engine.findDocs(self.collection, query).onDelete()
                }
            }
        }
        const source = this.authFylo.fylo.engine.findDocs(this.collection, query)
        return {
            async *[Symbol.asyncIterator]() {
                await self.authFylo._authorize({ action: 'doc:find', collection: self.collection })
                for await (const value of source) {
                    const filtered = await filterEnvelope(self.authFylo, self.collection, value)
                    if (filtered !== undefined) yield filtered
                }
            },
            async *collect() {
                await self.authFylo._authorize({ action: 'doc:find', collection: self.collection })
                for await (const value of source.collect()) {
                    const filtered = await filterEnvelope(self.authFylo, self.collection, value)
                    if (filtered !== undefined) yield filtered
                }
            },
            async *onDelete() {
                await self.authFylo._authorize({ action: 'doc:find', collection: self.collection })
                yield* source.onDelete()
            }
        }
    }
    findDeleted(query = {}) {
        const self = this
        const source = this.authFylo.fylo.engine.findDeletedDocs(this.collection, query)
        return {
            async *[Symbol.asyncIterator]() {
                await self.authFylo._authorize({ action: 'doc:find', collection: self.collection })
                yield* source
            },
            async *collect() {
                yield* source.collect()
            }
        }
    }
    /** @param {Record<string, any>} data */
    async put(data) {
        await this.authFylo._authorize({
            action: 'doc:create',
            collection: this.collection,
            docId: this.authFylo.firstDocId(data),
            data
        })
        return await new CollectionFacade(this.authFylo.fylo, this.collection).put(data)
    }
    /** @param {Record<string, any>[]} batch */
    async batchPut(batch) {
        for (const data of batch)
            await this.authFylo._authorize({
                action: 'doc:create',
                collection: this.collection,
                data
            })
        return await new CollectionFacade(this.authFylo.fylo, this.collection).batchPut(batch)
    }
    /** @param {TTIDValue} id @param {Record<string, any>} patch @param {Record<string, any>} [oldDoc] */
    async patch(id, patch, oldDoc = {}) {
        let existing = oldDoc[id]
        if (!existing) {
            const fetched = await this.authFylo.fylo.engine.getDoc(this.collection, id).once()
            existing = fetched[id]
        }
        if (!existing)
            throw new FyloAuthError({
                auth: this.authFylo.auth,
                action: 'doc:update',
                collection: this.collection,
                docId: id
            })
        await this.authFylo._authorize({
            action: 'doc:update',
            collection: this.collection,
            docId: id,
            data: patch,
            existing
        })
        return await this.authFylo.fylo.executePatchDocDirect(
            this.collection,
            { [id]: patch },
            oldDoc
        )
    }
    /** @param {StoreUpdate} update */
    async patchMany(update) {
        let count = 0
        for await (const value of this.find(update.$where ?? {}).collect()) {
            if (typeof value !== 'object' || value === null || Array.isArray(value)) continue
            const entry = Object.entries(value)[0]
            if (!entry) continue
            const [id, existing] = entry
            try {
                await this.authFylo._authorize({
                    action: 'doc:update',
                    collection: this.collection,
                    docId: id,
                    data: update.$set,
                    existing: /** @type {Record<string, any>} */ (existing)
                })
            } catch (err) {
                if (err instanceof FyloAuthError) continue
                throw err
            }
            await this.authFylo.fylo.executePatchDocDirect(
                this.collection,
                { [id]: update.$set },
                { [id]: /** @type {Record<string, any>} */ (existing) }
            )
            count++
        }
        return count
    }
    /** @param {TTIDValue} id */
    async ['delete'](id) {
        const fetched = await this.authFylo.fylo.engine.getDoc(this.collection, id).once()
        const existing = fetched[id]
        if (!existing)
            throw new FyloAuthError({
                auth: this.authFylo.auth,
                action: 'doc:delete',
                collection: this.collection,
                docId: id
            })
        await this.authFylo._authorize({
            action: 'doc:delete',
            collection: this.collection,
            docId: id,
            existing: /** @type {Record<string, any>} */ (existing)
        })
        return await this.authFylo.fylo.engine.deleteDocument(this.collection, id)
    }
    /** @param {StoreDelete} query */
    async deleteMany(query) {
        let count = 0
        for await (const value of this.find(query).collect()) {
            if (typeof value !== 'object' || value === null || Array.isArray(value)) continue
            const entry = Object.entries(value)[0]
            if (!entry) continue
            const [id, existing] = entry
            try {
                await this.authFylo._authorize({
                    action: 'doc:delete',
                    collection: this.collection,
                    docId: id,
                    existing: /** @type {Record<string, any>} */ (existing)
                })
            } catch (err) {
                if (err instanceof FyloAuthError) continue
                throw err
            }
            await this.authFylo.fylo.engine.deleteDocument(this.collection, id)
            count++
        }
        return count
    }
    /** @param {TTIDValue} id */
    async restore(id) {
        await this.authFylo._authorize({
            action: 'doc:update',
            collection: this.collection,
            docId: id
        })
        return await this.authFylo.fylo.engine.restoreDocument(this.collection, id)
    }
    async *export() {
        await this.authFylo._authorize({ action: 'bulk:export', collection: this.collection })
        for await (const doc of this.authFylo.fylo.engine.exportBulkData(this.collection))
            if (await this.authFylo._isVisible(this.collection, doc)) yield doc
    }
    /** @param {URL} url @param {number | ImportBulkDataOptions} [limitOrOptions] */
    async import(url, limitOrOptions) {
        await this.authFylo._authorize({ action: 'bulk:import', collection: this.collection })
        return await this.authFylo.fylo.importBulkData(this.collection, url, limitOrOptions)
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

/**
 * A `Fylo` instance accessed through its collection-facade Proxy: real methods
 * plus dynamic `fylo[collection]` access returning a `CollectionFacade`.
 * @typedef {Fylo & { [collection: string]: CollectionFacade }} FyloCollections
 */

/**
 * An RLS-scoped `AuthenticatedFylo` accessed through its collection-facade
 * Proxy: real methods plus dynamic `scoped[collection]` access.
 * @typedef {AuthenticatedFylo & { [collection: string]: AuthenticatedCollectionFacade }} AuthenticatedFyloCollections
 */
