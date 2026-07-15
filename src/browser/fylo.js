import { BrowserCore } from './core/engine.js'
import { runBrowserRequest } from './core/protocol.js'
import { createMemoryFilesystem } from './core/memory-filesystem.js'
import { createOpfsFilesystem } from './opfs-filesystem.js'
import { createWorkerClient } from './worker/client.js'
import { CollectionNotFoundError } from '../core/collection.js'

/**
 * @typedef {import('./core/filesystem.js').FyloFilesystem} FyloFilesystem
 * @typedef {import('./core/types.js').BrowserRequest} BrowserRequest
 * @typedef {import('./core/types.js').BrowserCoreOptions} BrowserCoreOptions
 * @typedef {import('./worker/client.js').FyloWorkerClient} FyloWorkerClient
 * @typedef {import('../types/vendor.js').TTID} TTIDValue
 * @typedef {import('../query/types.js').StoreDelete<Record<string, any>>} StoreDelete
 * @typedef {import('../query/types.js').StoreQuery<Record<string, any>>} StoreQuery
 * @typedef {import('../query/types.js').StoreUpdate<Record<string, any>>} StoreUpdate
 * @typedef {ReturnType<FyloBrowser['findDocs']>} BrowserFindResult
 * @typedef {ReturnType<FyloBrowser['findDeletedDocs']>} BrowserDeletedFindResult
 * @typedef {PromiseLike<TTIDValue> & { metadata(record: Record<string, any>): Promise<TTIDValue> }} BrowserMetadataPutOperation
 * @typedef {((data: Record<string, any>) => Promise<TTIDValue>) &
 *   ((id: TTIDValue, data: Record<string, any>) => BrowserMetadataPutOperation) &
 *   ((id: TTIDValue) => { metadata(record: Record<string, any>): Promise<TTIDValue> }) & {
 *   batch(batch: Record<string, any>[]): Promise<TTIDValue[]>
 * }} BrowserCollectionPut
 * @typedef {((id: TTIDValue, patch: Record<string, any>, oldDoc?: Record<TTIDValue, Record<string, any>>) => Promise<TTIDValue>) & {
 *   many(update: StoreUpdate): Promise<number>
 * }} BrowserCollectionPatch
 * @typedef {((id: TTIDValue) => Promise<void>) & {
 *   many(query: StoreDelete): Promise<number>
 * }} BrowserCollectionDelete
 * @typedef {((query?: StoreQuery) => BrowserFindResult) & {
 *   deleted(query?: StoreQuery): BrowserDeletedFindResult
 * }} BrowserCollectionFind
 */

/** @param {(record: Record<string, any> | undefined, present: boolean) => Promise<TTIDValue>} write @param {(record: Record<string, any>) => Promise<TTIDValue>} writeMetadata */
function browserMetadataPutOperation(write, writeMetadata) {
    /** @type {Record<string, any> | undefined} */
    let metadata
    let hasMetadata = false
    /** @type {Promise<TTIDValue> | undefined} */
    let operation
    const start = () => (operation ??= Promise.resolve().then(() => write(metadata, hasMetadata)))
    return {
        then(
            /** @type {(value: TTIDValue) => any} */ onFulfilled,
            /** @type {(reason: any) => any} */ onRejected
        ) {
            return start().then(onFulfilled, onRejected)
        },
        async metadata(/** @type {Record<string, any>} */ record) {
            if (!operation) {
                metadata = record
                hasMetadata = true
                return await start()
            }
            await start()
            return await writeMetadata(record)
        }
    }
}

/**
 * Browser-facing FYLO runtime backed by OPFS or an injected VFS.
 */
export class FyloBrowser {
    /** @type {BrowserCore | null} */
    core
    /** @type {FyloWorkerClient | null} */
    worker
    /**
     * @param {{ fs?: FyloFilesystem, storage?: 'memory' | 'opfs', namespace?: string, root?: string, worker?: boolean, worm?: BrowserCoreOptions['worm'] }=} options
     */
    constructor(options = {}) {
        this.namespace = options.namespace ?? 'fylo'
        this.root = options.root ?? '/'
        this.worker = shouldUseWorker(options.worker ?? true)
            ? createWorkerClient({
                  namespace: this.namespace,
                  storage: options.storage ?? 'opfs',
                  root: this.root,
                  worm: options.worm
              })
            : null
        const fs =
            options.fs ??
            (options.storage === 'opfs'
                ? createOpfsFilesystem({ namespace: this.namespace })
                : createMemoryFilesystem())
        this.core = this.worker
            ? null
            : new BrowserCore({ fs, root: this.root, worm: options.worm })
        this.sql = this.createSqlTag()
        const reserved = new Set([
            'then',
            'constructor',
            'prototype',
            ...Object.getOwnPropertyNames(Object.prototype),
            ...Object.getOwnPropertyNames(FyloBrowser.prototype),
            ...Object.getOwnPropertyNames(this)
        ])
        return new Proxy(this, {
            get(target, prop, receiver) {
                if (typeof prop === 'symbol') return Reflect.get(target, prop, receiver)
                if (reserved.has(prop)) return Reflect.get(target, prop, receiver)
                return new BrowserCollectionFacade(target, prop)
            }
        })
    }

    /** @returns {Promise<void>} */
    async ready() {
        await this.core?.ready()
    }

    /** @returns {Promise<void>} */
    async close() {
        await this.worker?.close()
        await this.core?.close()
    }

    /** @returns {(strings: TemplateStringsArray, ...values: unknown[]) => Promise<unknown>} */
    createSqlTag() {
        return async (strings, ...values) => {
            let statement = strings[0] ?? ''
            for (let index = 0; index < values.length; index++) {
                statement += FyloBrowser.sqlValue(values[index]) + (strings[index + 1] ?? '')
            }
            return await this._sql(statement)
        }
    }

    /** @returns {Record<string, BrowserCollectionFacade>} */
    createCollectionProxy() {
        const fylo = this
        const reserved = new Set([
            'then',
            'db',
            'constructor',
            'prototype',
            'toString',
            'valueOf',
            ...Object.getOwnPropertyNames(Object.prototype),
            ...Object.getOwnPropertyNames(FyloBrowser.prototype),
            ...Object.getOwnPropertyNames(fylo)
        ])
        return /** @type {Record<string, BrowserCollectionFacade>} */ (
            new Proxy(
                {},
                {
                    get(_target, prop) {
                        if (typeof prop === 'symbol') return undefined
                        if (reserved.has(prop)) {
                            throw new Error(
                                `Collection name collides with reserved db property: ${prop}`
                            )
                        }
                        return new BrowserCollectionFacade(fylo, prop)
                    },
                    has(_target, prop) {
                        return typeof prop === 'string' && !reserved.has(prop)
                    }
                }
            )
        )
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
        if (typeof value === 'object') throw new Error('SQL parameters must be scalar values')
        return `'${String(value).replaceAll("'", "''")}'`
    }

    /** @param {string} statement @returns {Promise<unknown>} */
    async _sql(statement) {
        return await this.dispatch({ op: 'executeSQL', sql: statement })
    }

    /** @param {BrowserRequest} request @returns {Promise<import('./core/types.js').BrowserSuccessResponse | import('./core/types.js').BrowserErrorResponse>} */
    async request(request) {
        if (this.worker) return await this.worker.envelope(request)
        return await runBrowserRequest(this, request)
    }

    /**
     * @param {BrowserRequest} request
     * @returns {Promise<unknown>}
     */
    async dispatch(request) {
        if (this.worker) return await this.worker.request(request)
        if (!this.core) throw new Error('FYLO browser runtime is not initialised')
        const response = await runBrowserRequest(this.core, request)
        if (!response.ok) throw browserProtocolError(response.error)
        return response.result
    }

    /** @param {string} collection @returns {Promise<void>} */
    async createCollection(collection) {
        await this.dispatch({ op: 'createCollection', collection })
    }

    /** @param {string} collection @returns {Promise<void>} */
    async dropCollection(collection) {
        await this.dispatch({ op: 'dropCollection', collection })
    }

    /** @param {string} collection @returns {Promise<unknown>} */
    async inspectCollection(collection) {
        return await this.dispatch({ op: 'inspectCollection', collection })
    }

    /** @param {string} collection @returns {Promise<unknown>} */
    async rebuildCollection(collection) {
        return await this.dispatch({ op: 'rebuildCollection', collection })
    }

    /** @param {string} collection @param {string} id @param {boolean} [onlyId] */
    getDoc(collection, id, onlyId = false) {
        const fylo = this
        if (!this.worker && this.core) return this.core.getDoc(collection, id, onlyId)
        return {
            async *[Symbol.asyncIterator]() {
                const doc = await this.once()
                if (doc && (typeof doc === 'string' || Object.keys(doc).length > 0)) yield doc
            },
            async once() {
                return await fylo.dispatch({ op: 'getDoc', collection, id, onlyId })
            },
            async *onDelete() {}
        }
    }

    /** @param {string} collection @param {string} id @param {boolean} [onlyId] @returns {Promise<unknown>} */
    async getLatest(collection, id, onlyId = false) {
        return await this.dispatch({ op: 'getLatest', collection, id, onlyId })
    }

    /** @param {string} collection @param {string} id */
    async getDocMeta(collection, id) {
        return await this.dispatch({ op: 'getMeta', collection, id })
    }

    /** @param {string} collection @param {string} id @param {Record<string, any>} meta */
    async setDocMetaRecord(collection, id, meta) {
        return await this.dispatch({ op: 'setMeta', collection, id, meta })
    }

    /** @param {string} collection @param {Record<string, any>} [query] */
    findDocs(collection, query = {}) {
        const fylo = this
        if (!this.worker && this.core) return this.core.findDocs(collection, query)
        return {
            async *[Symbol.asyncIterator]() {
                yield* this.collect()
            },
            async *collect() {
                const docs = await fylo.dispatch({ op: 'findDocs', collection, query })
                if (Array.isArray(docs)) {
                    for (const id of docs) yield id
                    return
                }
                for (const [id, doc] of Object.entries(/** @type {Record<string, any>} */ (docs))) {
                    yield { [id]: doc }
                }
            },
            async *onDelete() {}
        }
    }

    /** @param {string} collection @param {Record<string, any>} [query] */
    findDeletedDocs(collection, query = {}) {
        const fylo = this
        if (!this.worker && this.core) return this.core.findDeletedDocs(collection, query)
        return {
            async *[Symbol.asyncIterator]() {
                yield* this.collect()
            },
            async *collect() {
                const docs = await fylo.dispatch({ op: 'findDeletedDocs', collection, query })
                if (Array.isArray(docs)) {
                    for (const id of docs) yield id
                    return
                }
                for (const [id, doc] of Object.entries(/** @type {Record<string, any>} */ (docs))) {
                    yield { [id]: doc }
                }
            }
        }
    }

    /** @param {Record<string, any>} join @returns {Promise<unknown>} */
    async join(join) {
        return await this.dispatch({
            op: 'joinDocs',
            join: /** @type {import('../query/types.js').StoreJoin<Record<string, any>, Record<string, any>>} */ (
                join
            )
        })
    }

    /** @param {string} collection @param {Record<string, any>} data @param {Record<string, any>=} meta @param {boolean} [metaPresent] @returns {Promise<string>} */
    async putData(collection, data, meta, metaPresent = arguments.length >= 3) {
        return /** @type {string} */ (
            await this.dispatch({
                op: 'putData',
                collection,
                data,
                ...(metaPresent ? { meta } : {})
            })
        )
    }

    /** @param {string} collection @param {Record<string, any>[]} batch @returns {Promise<string[]>} */
    async batchPutData(collection, batch) {
        return /** @type {string[]} */ (
            await this.dispatch({ op: 'batchPutData', collection, batch })
        )
    }

    /** @param {string} collection @param {Record<string, any>} newDoc @param {Record<string, any>} [oldDoc] @returns {Promise<string>} */
    async patchDoc(collection, newDoc, oldDoc = {}) {
        return /** @type {string} */ (
            await this.dispatch({ op: 'patchDoc', collection, newDoc, oldDoc })
        )
    }

    /** @param {string} collection @param {Record<string, any>} update @returns {Promise<number>} */
    async patchDocs(collection, update) {
        return /** @type {number} */ (
            await this.dispatch({
                op: 'patchDocs',
                collection,
                update: /** @type {import('../query/types.js').StoreUpdate<Record<string, any>>} */ (
                    update
                )
            })
        )
    }

    /** @param {string} collection @param {string} id @returns {Promise<void>} */
    async delDoc(collection, id) {
        await this.dispatch({ op: 'delDoc', collection, id })
    }

    /** @param {string} collection @param {string} id @returns {Promise<string>} */
    async restoreDoc(collection, id) {
        const result = /** @type {{ id?: string } | string} */ (
            await this.dispatch({ op: 'restoreDoc', collection, id })
        )
        return typeof result === 'string' ? result : String(result.id)
    }

    /** @param {string} collection @param {Record<string, any>} query @returns {Promise<number>} */
    async delDocs(collection, query) {
        return /** @type {number} */ (
            await this.dispatch({ op: 'delDocs', collection, delete: query })
        )
    }

    /**
     * @param {string} collection
     * @param {(event: import('./core/types.js').BrowserEvent) => void} listener
     * @returns {() => void}
     */
    subscribe(collection, listener) {
        if (this.worker) return this.worker.subscribe(collection, listener)
        if (!this.core) throw new Error('FYLO browser runtime is not initialised')
        return this.core.subscribe(collection, listener)
    }
}

export class BrowserCollectionFacade {
    /** @type {FyloBrowser} */
    fylo
    /** @type {string} */
    collection
    /** @type {BrowserCollectionPut} */
    put
    /** @type {BrowserCollectionPatch} */
    patch
    /** @type {BrowserCollectionDelete} */
    delete
    /** @type {BrowserCollectionFind} */
    find

    /** @param {FyloBrowser} fylo @param {string} collection */
    constructor(fylo, collection) {
        this.fylo = fylo
        this.collection = collection
        const self = this
        const put = /** @type {BrowserCollectionPut} */ (
            /** @type {unknown} */ (
                function (
                    /** @type {Record<string, any> | string} */ dataOrId,
                    /** @type {Record<string, any>} */ data
                ) {
                    if (typeof dataOrId === 'string') {
                        const id = dataOrId
                        if (arguments.length === 1) {
                            return {
                                metadata: async (/** @type {Record<string, any>} */ record) =>
                                    await self.fylo.setDocMetaRecord(self.collection, id, record)
                            }
                        }
                        return browserMetadataPutOperation(
                            async (record, present) =>
                                await self.fylo.putData(
                                    self.collection,
                                    { [id]: data },
                                    record,
                                    present
                                ),
                            async (record) =>
                                await self.fylo
                                    .setDocMetaRecord(self.collection, id, record)
                                    .then(() => id)
                        )
                    }
                    return self.fylo.putData(
                        self.collection,
                        /** @type {Record<string, any>} */ (dataOrId)
                    )
                }
            )
        )
        put.batch = async (batch) => {
            return await self.fylo.batchPutData(self.collection, batch)
        }
        this.put = put

        const patch = /** @type {BrowserCollectionPatch} */ (
            /** @type {unknown} */ (
                function (
                    /** @type {string} */ id,
                    /** @type {Record<string, any>} */ patchData,
                    oldDoc = {}
                ) {
                    return self.fylo.patchDoc(self.collection, { [id]: patchData }, oldDoc)
                }
            )
        )
        patch.many = async (update) => {
            return await self.fylo.patchDocs(self.collection, update)
        }
        this.patch = patch

        const del = /** @type {BrowserCollectionDelete} */ (
            async (id) => {
                await self.fylo.delDoc(self.collection, id)
            }
        )
        del.many = async (query) => {
            return await self.fylo.delDocs(self.collection, query)
        }
        this.delete = del

        const find = /** @type {BrowserCollectionFind} */ (
            (query = {}) => {
                return self.fylo.findDocs(self.collection, query)
            }
        )
        find.deleted = (query = {}) => {
            return self.fylo.findDeletedDocs(self.collection, query)
        }
        this.find = find
    }

    /** @param {string} id @param {boolean} [onlyId] */
    get(id, onlyId = false) {
        return {
            ...this.fylo.getDoc(this.collection, id, onlyId),
            metadata: async () => await this.fylo.getDocMeta(this.collection, id)
        }
    }
    /** @param {string} id @param {boolean} [onlyId] */
    async latest(id, onlyId = false) {
        return await this.fylo.getLatest(this.collection, id, onlyId)
    }
    /** @param {string} id */
    async restore(id) {
        return await this.fylo.restoreDoc(this.collection, id)
    }

    async inspect() {
        return await this.fylo.inspectCollection(this.collection)
    }

    async rebuild() {
        return await this.fylo.rebuildCollection(this.collection)
    }

    async create() {
        await this.fylo.createCollection(this.collection)
    }

    async drop() {
        await this.fylo.dropCollection(this.collection)
    }

    /** @param {(event: import('./core/types.js').BrowserEvent) => void} listener */
    subscribe(listener) {
        return this.fylo.subscribe(this.collection, listener)
    }
}

/** @param {ConstructorParameters<typeof FyloBrowser>[0]} [options] @returns {FyloBrowser} */
export function createBrowserFylo(options) {
    return new FyloBrowser(options)
}

/**
 * @param {unknown} error
 * @returns {Error}
 */
function browserProtocolError(error) {
    if (!error || typeof error !== 'object') {
        return new Error('Unknown browser protocol error')
    }
    const err = /** @type {Record<string, unknown>} */ (error)
    if (err.code === 'FYLO_COLLECTION_NOT_FOUND') {
        return new CollectionNotFoundError(
            typeof err.message === 'string'
                ? err.message.replace(/^Collection not found: /, '')
                : ''
        )
    }
    const failure = new Error(typeof err.message === 'string' ? err.message : 'Request failed')
    failure.name = typeof err.name === 'string' ? err.name : 'Error'
    return failure
}

/**
 * @param {boolean} requested
 * @returns {boolean}
 */
function shouldUseWorker(requested) {
    return (
        requested &&
        typeof window !== 'undefined' &&
        typeof URL !== 'undefined' &&
        (typeof SharedWorker !== 'undefined' || typeof Worker !== 'undefined')
    )
}
