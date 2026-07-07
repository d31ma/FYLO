/**
 * @fileoverview Local-first synced FYLO client.
 *
 * `createSyncedClient({ serverUrl, token })` returns the same ergonomic facade as
 * the local browser client (`db.<collection>.put/get/find/...`), but every read
 * hits the local OPFS/memory store and every write is applied locally and then
 * reconciled with the backend by the {@link SyncEngine}. With no `serverUrl` (or
 * an unreachable one) it is a pure offline store.
 */
import { BrowserFyloClient } from '../client.js'
import { SyncEngine } from './engine.js'

const RESERVED = new Set([
    'then',
    'sync',
    'local',
    'ready',
    'close',
    'collection',
    'sql',
    '_sql',
    'constructor'
])

/**
 * @param {import('./engine.js').SyncOptions & import('../client.js').BrowserFyloOptions} [options]
 */
export function createSyncedClient(options = {}) {
    const local = new BrowserFyloClient(options)
    const engine = new SyncEngine(local, options)
    /** @type {Map<string, any>} memoized collection facades */
    const facades = new Map()

    /** @param {string} collection */
    function wrap(collection) {
        const cached = facades.get(collection)
        if (cached) return cached
        const col = local.collection(collection)
        engine.subscribe(collection)
        // Auto-create the local collection on first use (once), so a write never
        // fails just because this device hasn't materialized it yet.
        const ensured = col.create().catch(() => {})
        const wrapped = {
            // Reads and structural ops stay local.
            /** @param {string} id @param {boolean} [onlyId] */
            get: (id, onlyId) => col.get(id, onlyId),
            find: col.find,
            /** @param {string} id @param {boolean} [onlyId] */
            latest: (id, onlyId) => col.latest(id, onlyId),
            inspect: () => col.inspect(),
            rebuild: () => col.rebuild(),
            create: () => col.create(),
            drop: () => col.drop(),

            // Writes: local first, then capture for push.
            /** @param {Record<string, any>} data */
            async put(data) {
                await ensured
                const id = /** @type {string} */ (await col.put(data))
                // Keyed put `{ [id]: doc }` returns that id; unkeyed returns a fresh id.
                engine.capture(collection, id, id in data ? data[id] : data)
                return id
            },
            /** @param {string} id */
            async delete(id) {
                await ensured
                await col.delete(id)
                engine.capture(collection, id, null)
            },
            /** @param {string} id @param {Record<string, any>} patch @param {Record<string, any>} [oldDoc] */
            async patch(id, patch, oldDoc = {}) {
                await ensured
                const nextId = /** @type {string} */ (await col.patch(id, patch, oldDoc))
                const merged = /** @type {Record<string, any>} */ (await col.latest(nextId))
                engine.capture(collection, nextId, merged?.[nextId] ?? null)
                return nextId
            },
            /** @param {string} id */
            async restore(id) {
                await ensured
                const rid = /** @type {string} */ (await col.restore(id))
                const doc = /** @type {Record<string, any>} */ (await col.latest(rid))
                engine.capture(collection, rid, doc?.[rid] ?? null)
                return rid
            }
        }
        facades.set(collection, wrapped)
        return wrapped
    }

    const facade = {
        /** The sync engine — `start()`, `stop()`, `online`, `ping()`. */
        sync: engine,
        /** The underlying local (offline) client. */
        local,
        ready: () => local.ready(),
        close: () => {
            engine.stop()
            return local.close()
        },
        collection: wrap,
        // SQL runs against the local store (reads are local anyway; SQL-driven
        // writes are local-only and not pushed — use the collection facade to sync).
        /** @param {TemplateStringsArray} strings @param {unknown[]} values */
        sql: (strings, ...values) => local.sql(strings, ...values),
        /** @param {string} statement */
        _sql: (statement) => local._sql(statement)
    }

    return new Proxy(facade, {
        get(target, prop, receiver) {
            if (typeof prop === 'symbol' || RESERVED.has(prop) || prop in target) {
                return Reflect.get(target, prop, receiver)
            }
            return wrap(String(prop))
        }
    })
}

export { SyncEngine } from './engine.js'
