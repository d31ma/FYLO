/**
 * @typedef {{ name: string, size: number, pageSize: number, state: string, modifiedMs: number }} FileRecord
 * @typedef {{ name: string, owner: string, expiresAt: number }} OwnerRecord
 * @typedef {{ name: string, size: number, modifiedMs: number }} CopySummary
 * @typedef {{ held: boolean, release: () => void }} HeldLock
 * @typedef {Record<string, IDBObjectStore>} Stores
 */

export class IndexedDbStore {
    static DATABASE_NAME = 'fylo-vdbx'
    static DATABASE_VERSION = 1
    static FILES = 'files'
    static PAGES = 'pages'
    static OWNERS = 'owners'
    static LOCK_PREFIX = 'fylo-vdbx-idb:'
    static READY = 'ready'
    static LAST_PAGE = Number.MAX_SAFE_INTEGER

    /**
     * @param {string} name
     * @param {{ indexedDB?: IDBFactory, locks?: LockManager | null }} [options]
     * @returns {Promise<void>}
     */
    static async remove(name, options = {}) {
        const database = await IndexedDbStore.openDatabase(options.indexedDB)
        const lock = await IndexedDbStore.takeWebLock(name, options.locks)
        try {
            await IndexedDbStore.run(database, 'readwrite', (stores, fail) => {
                stores.owners.get(name).onsuccess = (event) => {
                    const holder = IndexedDbStore.resultOf(event)
                    if (IndexedDbStore.heldByOther(holder, '', lock.held)) {
                        return fail(IndexedDbStore.busy(name))
                    }
                    IndexedDbStore.deleteAll(stores, name)
                }
            })
        } finally {
            lock.release()
            database.close()
        }
    }

    /**
     * @param {{ indexedDB?: IDBFactory }} [options]
     * @returns {Promise<CopySummary[]>}
     */
    static async list(options = {}) {
        const database = await IndexedDbStore.openDatabase(options.indexedDB)
        try {
            /** @type {FileRecord[]} */
            let records = []
            await IndexedDbStore.run(database, 'readonly', (stores) => {
                stores.files.getAll().onsuccess = (event) => {
                    records = IndexedDbStore.resultOf(event)
                }
            })
            return IndexedDbStore.readyCopies(records)
        } finally {
            database.close()
        }
    }

    /**
     * @param {IDBFactory | undefined} given
     * @returns {Promise<IDBDatabase>}
     */
    static openDatabase(given) {
        const factory = given ?? globalThis.indexedDB
        if (!factory) {
            return Promise.reject(
                IndexedDbStore.coded(new Error('IndexedDB is unavailable here'), 'ENOSYS')
            )
        }
        return new Promise((resolve, reject) => {
            const request = factory.open(
                IndexedDbStore.DATABASE_NAME,
                IndexedDbStore.DATABASE_VERSION
            )
            request.onupgradeneeded = () => IndexedDbStore.createStores(request.result)
            request.onsuccess = () => resolve(request.result)
            request.onerror = () =>
                reject(
                    IndexedDbStore.coded(
                        new Error(`IndexedDB refused to open: ${request.error?.name}`),
                        'EIO'
                    )
                )
            request.onblocked = () =>
                reject(
                    IndexedDbStore.coded(
                        new Error('IndexedDB is blocked by an older version'),
                        'EBUSY'
                    )
                )
        })
    }

    /** @param {IDBDatabase} database */
    static createStores(database) {
        database.createObjectStore(IndexedDbStore.FILES, { keyPath: 'name' })
        database.createObjectStore(IndexedDbStore.PAGES)
        database.createObjectStore(IndexedDbStore.OWNERS, { keyPath: 'name' })
    }

    /**
     * @param {string} name
     * @param {LockManager | null | undefined} given
     * @returns {Promise<HeldLock>}
     */
    static async takeWebLock(name, given) {
        const locks = given === undefined ? globalThis.navigator?.locks : given
        if (!locks) return { held: false, release: () => {} }
        return new Promise((resolve, reject) => {
            locks
                .request(
                    IndexedDbStore.LOCK_PREFIX + name,
                    { mode: 'exclusive', ifAvailable: true },
                    (lock) => IndexedDbStore.holdLock(name, lock, resolve, reject)
                )
                .catch(reject)
        })
    }

    /**
     * @param {string} name
     * @param {Lock | null} lock
     * @param {(value: HeldLock) => void} resolve
     * @param {(error: Error) => void} reject
     * @returns {Promise<void> | undefined}
     */
    static holdLock(name, lock, resolve, reject) {
        if (!lock) {
            reject(IndexedDbStore.busy(name))
            return undefined
        }
        return new Promise((release) => resolve({ held: true, release: () => release(undefined) }))
    }

    /**
     * @param {OwnerRecord | undefined} holder
     * @param {string} owner
     * @param {boolean} lockHeld
     * @returns {boolean}
     */
    static heldByOther(holder, owner, lockHeld) {
        if (!holder || holder.owner === owner || lockHeld) return false
        return holder.expiresAt > Date.now()
    }

    /**
     * @param {string} name
     * @returns {IDBKeyRange}
     */
    static pagesOf(name) {
        return IDBKeyRange.bound([name, 0], [name, IndexedDbStore.LAST_PAGE])
    }

    /**
     * @param {Stores} stores
     * @param {string} name
     */
    static deleteAll(stores, name) {
        stores.pages.delete(IndexedDbStore.pagesOf(name))
        stores.files.delete(name)
        stores.owners.delete(name)
    }

    /**
     * @param {FileRecord[]} records
     * @returns {CopySummary[]}
     */
    static readyCopies(records) {
        return records
            .filter((record) => record.state === IndexedDbStore.READY)
            .map((record) => ({
                name: record.name,
                size: record.size,
                modifiedMs: record.modifiedMs
            }))
            .sort((a, b) => b.modifiedMs - a.modifiedMs)
    }

    /**
     * @param {IDBDatabase} database
     * @param {IDBTransactionMode} mode
     * @param {(stores: Stores, fail: (error: Error) => void) => void} body
     * @returns {Promise<void>}
     */
    static run(database, mode, body) {
        return new Promise((resolve, reject) => {
            const names = [IndexedDbStore.FILES, IndexedDbStore.PAGES, IndexedDbStore.OWNERS]
            const transaction = database.transaction(names, mode, { durability: 'strict' })
            /** @type {Error | null} */
            let reason = null
            const fail = (/** @type {Error} */ error) => {
                reason = error
                transaction.abort()
            }
            transaction.oncomplete = () => resolve()
            transaction.onabort = () =>
                reject(
                    reason ?? transaction.error ?? new Error('IndexedDB aborted the transaction')
                )
            body(IndexedDbStore.storesOf(transaction, names), fail)
        })
    }

    /**
     * @param {IDBTransaction} transaction
     * @param {string[]} names
     * @returns {Stores}
     */
    static storesOf(transaction, names) {
        return Object.fromEntries(names.map((name) => [name, transaction.objectStore(name)]))
    }

    /**
     * @param {Event} event
     * @returns {any}
     */
    static resultOf(event) {
        return /** @type {IDBRequest} */ (event.target).result
    }

    /**
     * @param {string} name
     * @returns {Error}
     */
    static busy(name) {
        return IndexedDbStore.coded(new Error(`${name} is open in another tab or window`), 'EBUSY')
    }

    /**
     * @param {Error} error
     * @param {string} code
     * @returns {Error}
     */
    static coded(error, code) {
        return Object.assign(error, { code })
    }
}
