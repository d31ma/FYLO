import { createBrowserFylo } from './fylo.js'
import { hasOpfs } from './opfs-filesystem.js'

/**
 * @typedef {Parameters<typeof createBrowserFylo>[0]} BrowserFyloOptions
 * @typedef {import('./core/types.js').BrowserEvent} BrowserEvent
 */

const RESERVED = new Set([
    'then',
    'browser',
    'collection',
    'configure',
    'close',
    'ready',
    'request',
    'sql',
    '_sql',
    'inspect',
    'rebuild',
    'create',
    'drop',
    'toString',
    'valueOf',
    'constructor',
    'prototype'
])

/**
 * @param {BrowserFyloOptions} [options]
 * @returns {Required<Pick<BrowserFyloOptions, 'storage' | 'worker'>> & BrowserFyloOptions}
 */
function normalizeOptions(options = {}) {
    return {
        ...options,
        storage: options.storage ?? (hasOpfs(globalThis.navigator) ? 'opfs' : 'memory'),
        worker: options.worker ?? typeof window !== 'undefined'
    }
}

/**
 * Root-level ergonomic FYLO browser client. Collection names are exposed directly
 * on the instance (`fylo.users.put(...)`) while reserved helper names remain
 * available on the root.
 */
export class BrowserFyloClient {
    /**
     * @param {BrowserFyloOptions} [options]
     */
    constructor(options = {}) {
        this.options = normalizeOptions(options)
        this.browser = createBrowserFylo(this.options)
        /** @type {Promise<void> | null} */
        this.readyPromise = null
        /** @type {Map<string, BrowserDirectCollection>} */
        this.collections = new Map()
        /** @type {(strings: TemplateStringsArray, ...values: unknown[]) => Promise<unknown>} */
        this.sql = async (strings, ...values) => {
            await this.ready()
            return await this.browser.sql(strings, ...values)
        }
    }

    /** @returns {Promise<void>} */
    async ready() {
        this.readyPromise ??= this.browser.ready()
        await this.readyPromise
    }

    /** @returns {Promise<void>} */
    async close() {
        await this.browser.close()
    }

    /**
     * Execute a raw SQL string (the `.sql` template tag delegates here). Mirrors
     * the server client so both engines expose the same SQL surface.
     * @param {string} statement
     * @returns {Promise<unknown>}
     */
    async _sql(statement) {
        await this.ready()
        return await this.browser._sql(statement)
    }

    /** @param {import('./core/types.js').BrowserRequest} request */
    async request(request) {
        await this.ready()
        return await this.browser.request(request)
    }

    /** @param {string} collection */
    collection(collection) {
        const existing = this.collections.get(collection)
        if (existing) return existing
        const facade = new BrowserDirectCollection(this, collection)
        this.collections.set(collection, facade)
        return facade
    }

    /** @param {string} collection */
    async createCollection(collection) {
        await this.ready()
        await this.browser.createCollection(collection)
    }

    /** @param {string} collection */
    async dropCollection(collection) {
        await this.ready()
        await this.browser.dropCollection(collection)
    }

    /** @param {string} collection */
    async inspectCollection(collection) {
        await this.ready()
        return await this.browser.inspectCollection(collection)
    }

    /** @param {string} collection */
    async rebuildCollection(collection) {
        await this.ready()
        return await this.browser.rebuildCollection(collection)
    }
}

/** @param {(record: Record<string, any> | undefined, present: boolean) => Promise<string>} write @param {(record: Record<string, any>) => Promise<string>} writeMetadata */
function directMetadataPutOperation(write, writeMetadata) {
    /** @type {Record<string, any> | undefined} */
    let metadata
    let hasMetadata = false
    /** @type {Promise<string> | undefined} */
    let operation
    const start = () => (operation ??= Promise.resolve().then(() => write(metadata, hasMetadata)))
    return {
        then(
            /** @type {(value: string) => any} */ onFulfilled,
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

export class BrowserDirectCollection {
    /** @type {any} */
    find

    /**
     * @param {BrowserFyloClient} host
     * @param {string} collection
     */
    constructor(host, collection) {
        this.host = host
        this.collection = collection
        const self = this
        const find = /** @type {any} */ ((query = {}) => self.findActive(query))
        find.deleted = (query = {}) => self.findDeleted(query)
        this.find = find
    }

    async create() {
        await this.host.createCollection(this.collection)
    }

    async drop() {
        await this.host.dropCollection(this.collection)
    }

    async inspect() {
        return await this.host.inspectCollection(this.collection)
    }

    async rebuild() {
        return await this.host.rebuildCollection(this.collection)
    }

    /** @param {import('./core/types.js').BrowserOperation} op @param {Record<string, any>} [fields] */
    async #req(op, fields = {}) {
        await this.host.ready()
        const res = await this.host.request({ op, collection: this.collection, ...fields })
        if (!res.ok) throw new Error(res.error?.message ?? 'browser request failed')
        return res.result
    }

    /** @param {Record<string, any> | string} dataOrId @param {Record<string, any>=} data */
    put(dataOrId, data) {
        if (typeof dataOrId === 'string') {
            const id = dataOrId
            if (arguments.length === 1) {
                return {
                    metadata: (/** @type {Record<string, any>} */ record) =>
                        this.#req('setMeta', { id, meta: record })
                }
            }
            return directMetadataPutOperation(
                async (record, present) =>
                    /** @type {string} */ (
                        await this.#req('putData', {
                            data: { [id]: data },
                            ...(present ? { meta: record } : {})
                        })
                    ),
                async (record) => {
                    await this.#req('setMeta', { id, meta: record })
                    return id
                }
            )
        }
        return this.#req('putData', { data: dataOrId })
    }

    /** @param {Record<string, any>[]} batch */
    batchPut(batch) {
        return this.#req('batchPutData', { batch })
    }

    /** @param {string} id @param {Record<string, any>} patch @param {Record<string, any>} [oldDoc] */
    patch(id, patch, oldDoc = {}) {
        return this.#req('patchDoc', { newDoc: { [id]: patch }, oldDoc })
    }

    /** @param {Record<string, any>} update */
    patchMany(update) {
        return this.#req('patchDocs', { update })
    }

    /** @param {string} id */
    async delete(id) {
        await this.#req('delDoc', { id })
    }

    /** @param {Record<string, any>} query */
    deleteMany(query) {
        return this.#req('delDocs', { query })
    }

    /** @param {string} id */
    async restore(id) {
        const res = /** @type {{ id?: string } | undefined} */ (
            await this.#req('restoreDoc', { id })
        )
        return res?.id ?? res
    }

    /** @param {string} id @param {boolean} [onlyId] */
    get(id, onlyId = false) {
        const host = this.host
        const collection = this.collection
        return {
            async *[Symbol.asyncIterator]() {
                yield* this.collect()
            },
            async once() {
                const res = await host.request({ op: 'getDoc', collection, id, onlyId })
                if (!res.ok) throw new Error(res.error?.message ?? 'browser getDoc failed')
                return res.result
            },
            metadata: async () => await this.#req('getMeta', { id }),
            async *collect() {
                const doc = await this.once()
                if (doc && typeof doc === 'object' && Object.keys(doc).length > 0) yield doc
            },
            async *onDelete() {}
        }
    }

    /** @param {string} id @param {boolean} [onlyId] */
    async latest(id, onlyId = false) {
        return await this.#req('getLatest', { id, onlyId })
    }

    findActive(query = {}) {
        const host = this.host
        const collection = this.collection
        return {
            async *[Symbol.asyncIterator]() {
                yield* this.collect()
            },
            async *collect() {
                const res = await host.request({ op: 'findDocs', collection, query })
                if (!res.ok) throw new Error(res.error?.message ?? 'browser findDocs failed')
                const docs = res.result
                if (Array.isArray(docs)) {
                    for (const id of docs) yield id
                } else if (docs && typeof docs === 'object') {
                    for (const [id, doc] of Object.entries(docs)) yield { [id]: doc }
                }
            },
            async *onDelete() {}
        }
    }

    findDeleted(query = {}) {
        const host = this.host
        const collection = this.collection
        return {
            async *[Symbol.asyncIterator]() {
                yield* this.collect()
            },
            async *collect() {
                const res = await host.request({ op: 'findDeletedDocs', collection, query })
                if (!res.ok) throw new Error(res.error?.message ?? 'browser findDeletedDocs failed')
                const docs = res.result
                if (docs && typeof docs === 'object') {
                    for (const [id, doc] of Object.entries(docs)) {
                        yield { [id]: doc }
                    }
                }
            }
        }
    }

    /** @param {(event: BrowserEvent) => void} listener */
    subscribe(listener) {
        return this.host.browser.subscribe(this.collection, listener)
    }
}

/**
 * @param {BrowserFyloOptions} [options]
 * @returns {BrowserFyloClient & Record<string, BrowserDirectCollection>}
 */
export function createBrowserClient(options = {}) {
    const client = new BrowserFyloClient(options)
    return /** @type {BrowserFyloClient & Record<string, BrowserDirectCollection>} */ (
        new Proxy(client, {
            get(target, prop, receiver) {
                if (typeof prop === 'symbol') return Reflect.get(target, prop, receiver)
                if (RESERVED.has(prop) || prop in target) return Reflect.get(target, prop, receiver)
                return target.collection(prop)
            },
            has(target, prop) {
                return typeof prop === 'string'
                    ? !RESERVED.has(prop) || prop in target
                    : prop in target
            }
        })
    )
}

const fylo = createBrowserClient()

export { FyloBrowser, BrowserCollectionFacade, createBrowserFylo } from './fylo.js'
export { OpfsFilesystem, createOpfsFilesystem } from './opfs-filesystem.js'

export default fylo
