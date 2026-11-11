import { IndexedDbStore } from './indexed-db-store.mjs'

/**
 * @typedef {import('./indexed-db-store.mjs').FileRecord} FileRecord
 * @typedef {import('./indexed-db-store.mjs').OwnerRecord} OwnerRecord
 * @typedef {{ copied: number, total: number }} CopyProgress
 * @typedef {{
 *   copyFrom?: Blob,
 *   onProgress?: (progress: CopyProgress) => void,
 *   indexedDB?: IDBFactory,
 *   locks?: LockManager | null,
 *   leaseMs?: number,
 *   renewMs?: number
 * }} OpenOptions
 */

export class IndexedDbFile {
    static PAGE_SIZE = 16 * 1024
    static LEASE_MS = 6000
    static RENEW_MS = 2000
    static COPY_COMMIT_BYTES = 4 * 1024 * 1024
    static COPYING = 'copying'

    /**
     * @param {{
     *   database: IDBDatabase,
     *   name: string,
     *   owner: string,
     *   record: FileRecord,
     *   pages: Map<number, Uint8Array>,
     *   lease: { leaseMs: number, renewMs: number },
     *   releaseLock: () => void
     * }} parts
     */
    constructor({ database, name, owner, record, pages, lease, releaseLock }) {
        /** @type {IDBDatabase} */
        this.database = database
        /** @type {string} */
        this.name = name
        /** @type {string} */
        this.owner = owner
        /** @type {number} */
        this.pageSize = record.pageSize
        /** @type {number} */
        this.length = record.size
        /** @type {number} */
        this.shrunkTo = Infinity
        /** @type {number} */
        this.modified = record.modifiedMs
        /** @type {string} */
        this.state = record.state
        /** @type {Map<number, Uint8Array>} */
        this.pages = pages
        /** @type {Set<number>} */
        this.dirtyPages = new Set()
        /** @type {boolean} */
        this.recordDirty = false
        /** @type {Error | null} */
        this.failure = null
        /** @type {{ leaseMs: number, renewMs: number }} */
        this.lease = lease
        /** @type {() => void} */
        this.releaseLock = releaseLock
        /** @type {ReturnType<typeof setInterval> | undefined} */
        this.renewTimer = undefined
    }

    /**
     * @param {string} name
     * @param {OpenOptions} [options]
     * @returns {Promise<IndexedDbFile>}
     */
    static async open(name, options = {}) {
        const lease = IndexedDbFile.leaseOf(options)
        const database = await IndexedDbStore.openDatabase(options.indexedDB)
        const lock = await IndexedDbStore.takeWebLock(name, options.locks)
        const owner = IndexedDbFile.newOwnerId()
        try {
            const record = await IndexedDbFile.claim(
                database,
                name,
                owner,
                lock.held,
                options,
                lease
            )
            const pages = await IndexedDbFile.loadPages(database, name, Boolean(options.copyFrom))
            const file = new IndexedDbFile({
                database,
                name,
                owner,
                record,
                pages,
                lease,
                releaseLock: lock.release
            })
            await file.copyIn(options)
            file.startRenewing()
            return file
        } catch (error) {
            await IndexedDbFile.giveUp(database, name, owner, lock.release)
            throw error
        }
    }

    /** @returns {number} */
    size() {
        return this.length
    }

    /** @returns {number} */
    modifiedMs() {
        return this.modified
    }

    /**
     * @param {number} offset
     * @param {Uint8Array} into
     * @returns {number}
     */
    readAt(offset, into) {
        const end = Math.min(offset + into.length, this.length)
        let position = offset
        while (position < end) {
            position = this.readPagePart(position, end, into.subarray(position - offset))
        }
        return Math.max(0, end - offset)
    }

    /**
     * @param {number} offset
     * @param {Uint8Array} from
     * @returns {number}
     */
    writeAt(offset, from) {
        this.refuseWhenFailed()
        const end = offset + from.length
        let position = offset
        while (position < end) {
            position = this.writePagePart(position, end, from.subarray(position - offset))
        }
        this.length = Math.max(this.length, end)
        this.touch()
        return from.length
    }

    /** @param {number} length */
    truncate(length) {
        this.refuseWhenFailed()
        if (length < this.length) this.dropPagesFrom(length)
        this.shrunkTo = Math.min(this.shrunkTo, length)
        this.length = length
        this.touch()
    }

    /** @returns {boolean} */
    hasChanges() {
        return this.recordDirty
    }

    /** @returns {Promise<void>} */
    async commit() {
        this.refuseWhenFailed()
        if (!this.recordDirty) return
        try {
            await this.writeChanges()
        } catch (error) {
            this.failure = IndexedDbFile.commitFailure(error)
            throw this.failure
        }
    }

    /** @returns {Promise<void>} */
    async close() {
        clearInterval(this.renewTimer)
        try {
            await this.releaseLease()
        } finally {
            this.releaseLock()
            this.database.close()
        }
    }

    refuseWhenFailed() {
        if (this.failure) throw this.failure
    }

    touch() {
        this.modified = Date.now()
        this.recordDirty = true
    }

    /**
     * @param {number} position
     * @param {number} end
     * @param {Uint8Array} into
     * @returns {number}
     */
    readPagePart(position, end, into) {
        const index = Math.floor(position / this.pageSize)
        const start = position - index * this.pageSize
        const count = Math.min(end - position, this.pageSize - start)
        const page = this.pages.get(index)
        if (page) into.set(page.subarray(start, start + count))
        else into.fill(0, 0, count)
        return position + count
    }

    /**
     * @param {number} position
     * @param {number} end
     * @param {Uint8Array} from
     * @returns {number}
     */
    writePagePart(position, end, from) {
        const index = Math.floor(position / this.pageSize)
        const start = position - index * this.pageSize
        const count = Math.min(end - position, this.pageSize - start)
        this.pageForWrite(index).set(from.subarray(0, count), start)
        this.dirtyPages.add(index)
        return position + count
    }

    /**
     * @param {number} index
     * @returns {Uint8Array}
     */
    pageForWrite(index) {
        const found = this.pages.get(index)
        if (found) return found
        const page = new Uint8Array(this.pageSize)
        this.pages.set(index, page)
        return page
    }

    /** @param {number} length */
    dropPagesFrom(length) {
        const keep = Math.ceil(length / this.pageSize)
        for (const index of [...this.pages.keys()]) {
            if (index >= keep) this.pages.delete(index)
        }
        this.zeroTailOf(length)
    }

    /** @param {number} length */
    zeroTailOf(length) {
        const index = Math.floor(length / this.pageSize)
        const page = this.pages.get(index)
        if (!page) return
        page.fill(0, length - index * this.pageSize)
        this.dirtyPages.add(index)
    }

    /** @returns {Promise<void>} */
    async writeChanges() {
        const dirty = [...this.dirtyPages]
        const record = this.fileRecord()
        await IndexedDbStore.run(this.database, 'readwrite', (stores, fail) => {
            stores.owners.get(this.name).onsuccess = (event) => {
                const holder = IndexedDbStore.resultOf(event)
                if (holder?.owner !== this.owner) return fail(IndexedDbFile.lockLost(this.name))
                stores.owners.put(this.ownerRecord())
                this.deleteDroppedPages(stores.pages)
                this.putPages(stores.pages, dirty)
                stores.files.put(record)
            }
        })
        this.forgetCommitted(dirty)
    }

    /**
     * @param {IDBObjectStore} pages
     * @param {number[]} dirty
     */
    putPages(pages, dirty) {
        for (const index of dirty) this.putPage(pages, index)
    }

    /**
     * @param {IDBObjectStore} pages
     * @param {number} index
     */
    putPage(pages, index) {
        const page = this.pages.get(index)
        if (!page || IndexedDbFile.isZero(page)) {
            pages.delete([this.name, index])
            return
        }
        pages.put(page.buffer, [this.name, index])
    }

    /** @param {IDBObjectStore} pages */
    deleteDroppedPages(pages) {
        if (this.shrunkTo === Infinity) return
        const first = Math.ceil(this.shrunkTo / this.pageSize)
        pages.delete(IDBKeyRange.bound([this.name, first], [this.name, IndexedDbStore.LAST_PAGE]))
    }

    /** @param {number[]} committed */
    forgetCommitted(committed) {
        for (const index of committed) this.dirtyPages.delete(index)
        this.shrunkTo = Infinity
        this.recordDirty = this.dirtyPages.size > 0
    }

    /** @returns {FileRecord} */
    fileRecord() {
        return {
            name: this.name,
            size: this.length,
            pageSize: this.pageSize,
            state: this.state,
            modifiedMs: this.modified
        }
    }

    /** @returns {OwnerRecord} */
    ownerRecord() {
        return { name: this.name, owner: this.owner, expiresAt: Date.now() + this.lease.leaseMs }
    }

    /**
     * @param {OpenOptions} options
     * @returns {Promise<void>}
     */
    async copyIn(options) {
        if (!options.copyFrom) return
        const reader = options.copyFrom.stream().getReader()
        let copied = 0
        let uncommitted = 0
        for (let chunk = await reader.read(); !chunk.done; chunk = await reader.read()) {
            this.writeAt(copied, chunk.value)
            copied += chunk.value.length
            uncommitted = await this.commitCopyChunk(uncommitted + chunk.value.length)
            options.onProgress?.({ copied, total: options.copyFrom.size })
        }
        await this.finishCopy(copied, options.copyFrom.size)
    }

    /**
     * @param {number} uncommitted
     * @returns {Promise<number>}
     */
    async commitCopyChunk(uncommitted) {
        if (uncommitted < IndexedDbFile.COPY_COMMIT_BYTES) return uncommitted
        await this.commit()
        return 0
    }

    /**
     * @param {number} copied
     * @param {number} total
     * @returns {Promise<void>}
     */
    async finishCopy(copied, total) {
        if (copied !== total) throw new Error(`copied ${copied} of ${total} bytes`)
        this.state = IndexedDbStore.READY
        this.touch()
        await this.commit()
    }

    startRenewing() {
        this.renewTimer = setInterval(() => this.renew(), this.lease.renewMs)
    }

    /** @returns {Promise<void>} */
    async renew() {
        try {
            await this.writeLease()
        } catch (error) {
            clearInterval(this.renewTimer)
            this.failure = this.failure ?? IndexedDbFile.commitFailure(error)
        }
    }

    /** @returns {Promise<void>} */
    writeLease() {
        return IndexedDbStore.run(this.database, 'readwrite', (stores, fail) => {
            stores.owners.get(this.name).onsuccess = (event) => {
                const holder = IndexedDbStore.resultOf(event)
                if (holder?.owner !== this.owner) return fail(IndexedDbFile.lockLost(this.name))
                stores.owners.put(this.ownerRecord())
            }
        })
    }

    /** @returns {Promise<void>} */
    releaseLease() {
        return IndexedDbFile.dropLease(this.database, this.name, this.owner)
    }

    /**
     * @param {OpenOptions} options
     * @returns {{ leaseMs: number, renewMs: number }}
     */
    static leaseOf(options) {
        return {
            leaseMs: options.leaseMs ?? IndexedDbFile.LEASE_MS,
            renewMs: options.renewMs ?? IndexedDbFile.RENEW_MS
        }
    }

    /**
     * @param {IDBDatabase} database
     * @param {string} name
     * @param {string} owner
     * @param {boolean} lockHeld
     * @param {OpenOptions} options
     * @param {{ leaseMs: number }} lease
     * @returns {Promise<FileRecord>}
     */
    static async claim(database, name, owner, lockHeld, options, lease) {
        /** @type {FileRecord | undefined} */
        let record
        await IndexedDbStore.run(database, 'readwrite', (stores, fail) => {
            stores.owners.get(name).onsuccess = (event) => {
                const holder = IndexedDbStore.resultOf(event)
                if (IndexedDbStore.heldByOther(holder, owner, lockHeld))
                    return fail(IndexedDbStore.busy(name))
                stores.owners.put({ name, owner, expiresAt: Date.now() + lease.leaseMs })
                record = IndexedDbFile.startRecord(stores, name, options, fail)
            }
        })
        return /** @type {FileRecord} */ (
            await IndexedDbFile.existingRecord(database, name, record)
        )
    }

    /**
     * @param {Record<string, IDBObjectStore>} stores
     * @param {string} name
     * @param {OpenOptions} options
     * @param {(error: Error) => void} fail
     * @returns {FileRecord | undefined}
     */
    static startRecord(stores, name, options, fail) {
        if (!options.copyFrom) return undefined
        const record = {
            name,
            size: 0,
            pageSize: IndexedDbFile.PAGE_SIZE,
            state: IndexedDbFile.COPYING,
            modifiedMs: Date.now()
        }
        stores.pages.delete(IndexedDbStore.pagesOf(name))
        stores.files.put(record)
        return record
    }

    /**
     * @param {IDBDatabase} database
     * @param {string} name
     * @param {FileRecord | undefined} started
     * @returns {Promise<FileRecord>}
     */
    static async existingRecord(database, name, started) {
        if (started) return started
        /** @type {FileRecord | undefined} */
        let record
        await IndexedDbStore.run(database, 'readonly', (stores) => {
            stores.files.get(name).onsuccess = (event) => {
                record = IndexedDbStore.resultOf(event)
            }
        })
        return IndexedDbFile.openableRecord(name, record)
    }

    /**
     * @param {string} name
     * @param {FileRecord | undefined} record
     * @returns {FileRecord}
     */
    static openableRecord(name, record) {
        if (!record) throw IndexedDbStore.coded(new Error(`${name} does not exist`), 'ENOENT')
        if (record.state !== IndexedDbStore.READY) {
            throw IndexedDbStore.coded(
                new Error(`${name} is an unfinished copy; copy it again`),
                'EIO'
            )
        }
        return record
    }

    /**
     * @param {IDBDatabase} database
     * @param {string} name
     * @param {boolean} fresh
     * @returns {Promise<Map<number, Uint8Array>>}
     */
    static async loadPages(database, name, fresh) {
        /** @type {Map<number, Uint8Array>} */
        const pages = new Map()
        if (fresh) return pages
        const range = IndexedDbStore.pagesOf(name)
        /** @type {IDBValidKey[]} */
        let keys = []
        /** @type {ArrayBuffer[]} */
        let values = []
        await IndexedDbStore.run(database, 'readonly', (stores) => {
            stores.pages.getAllKeys(range).onsuccess = (event) => {
                keys = IndexedDbStore.resultOf(event)
            }
            stores.pages.getAll(range).onsuccess = (event) => {
                values = IndexedDbStore.resultOf(event)
            }
        })
        keys.forEach((key, position) =>
            pages.set(/** @type {number[]} */ (key)[1], new Uint8Array(values[position]))
        )
        return pages
    }

    /**
     * @param {IDBDatabase} database
     * @param {string} name
     * @param {string} owner
     * @param {() => void} releaseLock
     * @returns {Promise<void>}
     */
    static async giveUp(database, name, owner, releaseLock) {
        try {
            await IndexedDbFile.dropLease(database, name, owner)
        } finally {
            releaseLock()
            database.close()
        }
    }

    /**
     * @param {IDBDatabase} database
     * @param {string} name
     * @param {string} owner
     * @returns {Promise<void>}
     */
    static dropLease(database, name, owner) {
        return IndexedDbStore.run(database, 'readwrite', (stores) => {
            stores.owners.get(name).onsuccess = (event) => {
                const holder = IndexedDbStore.resultOf(event)
                if (holder?.owner === owner) stores.owners.delete(name)
            }
        })
    }

    /**
     * @param {Uint8Array} page
     * @returns {boolean}
     */
    static isZero(page) {
        const words = new Uint32Array(page.buffer, page.byteOffset, page.byteLength / 4)
        return words.every((word) => word === 0)
    }

    /**
     * @param {unknown} error
     * @returns {Error}
     */
    static commitFailure(error) {
        const failure = error instanceof Error ? error : new Error(String(error))
        if (/** @type {{ code?: string }} */ (failure).code) return failure
        const code = failure.name === 'QuotaExceededError' ? 'ENOSPC' : 'EIO'
        return IndexedDbStore.coded(
            new Error(`storing the working copy failed: ${failure.name}: ${failure.message}`),
            code
        )
    }

    /**
     * @param {string} name
     * @returns {Error}
     */
    static lockLost(name) {
        return IndexedDbStore.coded(
            new Error(`another tab took over ${name}; open it again`),
            'ELOCKLOST'
        )
    }

    /** @returns {string} */
    static newOwnerId() {
        const bytes = crypto.getRandomValues(new Uint8Array(16))
        return [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('')
    }
}
