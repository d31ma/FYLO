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
    'inspectCollection',
    'rebuildCollection',
    'createCollection',
    'dropCollection',
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
 * on the instance (`fylo.users.putData(...)`) while reserved helper names remain
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

export class BrowserDirectCollection {
    /**
     * @param {BrowserFyloClient} host
     * @param {string} collection
     */
    constructor(host, collection) {
        this.host = host
        this.collection = collection
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

    /** @param {Record<string, any>} data */
    async putData(data) {
        await this.host.ready()
        return await this.host.browser.putData(this.collection, data)
    }

    /** @param {Record<string, any>[]} batch */
    async batchPutData(batch) {
        await this.host.ready()
        return await this.host.browser.batchPutData(this.collection, batch)
    }

    /** @param {string} id @param {Record<string, any>} patch @param {Record<string, any>} [oldDoc] */
    async patchDoc(id, patch, oldDoc = {}) {
        await this.host.ready()
        return await this.host.browser.patchDoc(this.collection, { [id]: patch }, oldDoc)
    }

    /** @param {Record<string, any>} update */
    async patchDocs(update) {
        await this.host.ready()
        return await this.host.browser.patchDocs(this.collection, update)
    }

    /** @param {string} id */
    async delDoc(id) {
        await this.host.ready()
        await this.host.browser.delDoc(this.collection, id)
    }

    /** @param {Record<string, any>} query */
    async delDocs(query) {
        await this.host.ready()
        return await this.host.browser.delDocs(this.collection, query)
    }

    /** @param {string} id */
    async restoreDoc(id) {
        await this.host.ready()
        return await this.host.browser.restoreDoc(this.collection, id)
    }

    /** @param {string} id @param {boolean} [onlyId] */
    getDoc(id, onlyId = false) {
        const host = this.host
        const collection = this.collection
        return {
            async *[Symbol.asyncIterator]() {
                yield* this.collect()
            },
            async once() {
                await host.ready()
                return await host.browser.getDoc(collection, id, onlyId).once()
            },
            async *collect() {
                const doc = await this.once()
                if (doc && typeof doc === 'object' && Object.keys(doc).length > 0) yield doc
            },
            async *onDelete() {
                await host.ready()
                yield* host.browser.getDoc(collection, id, onlyId).onDelete()
            }
        }
    }

    /** @param {string} id @param {boolean} [onlyId] */
    async getLatest(id, onlyId = false) {
        await this.host.ready()
        return await this.host.browser.getLatest(this.collection, id, onlyId)
    }

    /** @param {Record<string, any>} [query] */
    findDocs(query = {}) {
        const host = this.host
        const collection = this.collection
        return {
            async *[Symbol.asyncIterator]() {
                yield* this.collect()
            },
            async *collect() {
                await host.ready()
                yield* host.browser.findDocs(collection, query).collect()
            },
            async *onDelete() {
                await host.ready()
                yield* host.browser.findDocs(collection, query).onDelete()
            }
        }
    }

    /** @param {Record<string, any>} [query] */
    findDeletedDocs(query = {}) {
        const host = this.host
        const collection = this.collection
        return {
            async *[Symbol.asyncIterator]() {
                yield* this.collect()
            },
            async *collect() {
                await host.ready()
                yield* host.browser.findDeletedDocs(collection, query).collect()
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
