import { mkdir, open, readdir, rm, stat } from 'node:fs/promises'
import path from 'node:path'
import { assertPathInside, validateDocId } from '../core/doc-id.js'
import { writeDurable } from './durable.js'
import { tryAcquireFileLock, tryReleaseFileLock, waitAcquireFileLock } from './fs-lock.js'

/**
 * @typedef {import('../types/vendor.js').TTID} TTIDValue
 * @typedef {import('./types.js').StorageEngine} StorageEngine
 * @typedef {import('./types.js').EventBus<Record<string, any>>} RecordEventBus
 */

/**
 * Minimal filesystem storage adapter used by higher-level collection
 * primitives.
 */
export class FilesystemStorage {
    /**
     * @param {string} target
     * @returns {Promise<string>}
     */
    async read(target) {
        return await Bun.file(target).text()
    }
    /**
     * @param {string} target
     * @param {string} data
     * @returns {Promise<void>}
     */
    async write(target, data) {
        await writeDurable(target, data)
    }
    /**
     * @param {string} target
     * @returns {Promise<void>}
     */
    async delete(target) {
        await rm(target, { recursive: true, force: true })
    }
    /**
     * Recursively lists files under a storage path. Missing roots are treated as empty.
     * @param {string} target
     * @returns {Promise<string[]>}
     */
    async list(target) {
        /** @type {string[]} */
        const results = []
        try {
            const entries = await readdir(target, { withFileTypes: true })
            for (const entry of entries) {
                const child = path.join(target, entry.name)
                if (entry.isDirectory()) {
                    results.push(...(await this.list(child)))
                } else {
                    results.push(child)
                }
            }
        } catch (err) {
            const error = /** @type {NodeJS.ErrnoException} */ (err)
            if (error.code !== 'ENOENT') throw err
        }
        return results
    }
    /**
     * @param {string} target
     * @returns {Promise<void>}
     */
    async mkdir(target) {
        await mkdir(target, { recursive: true })
    }
    /**
     * @param {string} target
     * @returns {Promise<void>}
     */
    async rmdir(target) {
        await rm(target, { recursive: true, force: true })
    }
    /**
     * @param {string} target
     * @returns {Promise<boolean>}
     */
    async exists(target) {
        try {
            await stat(target)
            return true
        } catch (err) {
            const error = /** @type {NodeJS.ErrnoException} */ (err)
            if (error.code === 'ENOENT') return false
            throw err
        }
    }
}
/**
 * Collection/document lock manager built on advisory filesystem lock files.
 */
export class FilesystemLockManager {
    /** @type {string} */
    root
    /** @type {StorageEngine} */
    storage
    /**
     * @param {string} root
     * @param {StorageEngine} storage
     */
    constructor(root, storage) {
        this.root = root
        this.storage = storage
    }
    /**
     * @param {string} collection
     * @param {TTIDValue} docId
     * @returns {string}
     */
    lockPath(collection, docId) {
        validateDocId(docId)
        const locksRoot = path.join(this.root, '.collections', collection, 'locks')
        const target = path.join(locksRoot, `${docId}.lock`)
        assertPathInside(locksRoot, target)
        return target
    }
    /**
     * Acquires an advisory filesystem lock for a document write lane.
     * Backed by an atomic single-file `wx` create; see fs-lock.js.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @param {string} owner
     * @param {number | import('./fs-lock.js').TryAcquireFileLockOptions} [ttlMsOrOptions]
     * @returns {Promise<boolean>}
     */
    async acquire(collection, docId, owner, ttlMsOrOptions = 30_000) {
        return await tryAcquireFileLock(this.lockPath(collection, docId), owner, ttlMsOrOptions)
    }
    /**
     * Releases a lock only when the current owner matches.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @param {string} owner
     * @returns {Promise<void>}
     */
    async release(collection, docId, owner) {
        await tryReleaseFileLock(this.lockPath(collection, docId), owner)
    }
    /**
     * @param {string} collection
     * @returns {string}
     */
    collectionLockPath(collection) {
        return path.join(this.root, '.collections', collection, 'locks', 'collection.lock')
    }
    /**
     * Blocking-acquires the collection-level write lock. Serializes
     * cross-process index mutations on the same collection.
     *
     * @param {string} collection
     * @param {string} owner
     * @param {object} [options]
     * @param {number} [options.ttlMs]
     * @param {number} [options.waitTimeoutMs]
     * @param {(info: { lockPath: string, newOwner: string, previousOwner?: string }) => void} [options.onTakeover]
     * @returns {Promise<void>}
     */
    async acquireCollectionWrite(collection, owner, options) {
        await waitAcquireFileLock(this.collectionLockPath(collection), owner, {
            ...options,
            ttlMs: 300_000,
            heartbeat: true
        })
    }
    /**
     * Releases the collection-level write lock if owner matches.
     * @param {string} collection
     * @param {string} owner
     * @returns {Promise<void>}
     */
    async releaseCollectionWrite(collection, owner) {
        await tryReleaseFileLock(this.collectionLockPath(collection), owner)
    }
}
/**
 * Append-only event journal used by collection listeners and queue mirroring.
 */
export class FilesystemEventBus {
    /** @type {string} */
    root
    /** @type {StorageEngine} */
    storage
    /**
     * @param {string} root
     * @param {StorageEngine} storage
     */
    constructor(root, storage) {
        this.root = root
        this.storage = storage
    }
    /**
     * @param {string} collection
     * @returns {string}
     */
    journalPath(collection) {
        return path.join(this.root, '.collections', collection, 'events', `${collection}.ndjson`)
    }
    /**
     * Appends an event to the collection journal.
     * @template {Record<string, any>} T
     * @param {string} collection
     * @param {T} event
     * @returns {Promise<void>}
     */
    async publish(collection, event) {
        const target = this.journalPath(collection)
        await mkdir(path.dirname(target), { recursive: true })
        const line = `${JSON.stringify(event)}\n`
        const handle = await open(target, 'a')
        try {
            await handle.write(line)
        } finally {
            await handle.close()
        }
    }
    /**
     * Continuously tails the collection journal.
     * @template {Record<string, any>} T
     * @param {string} collection
     * @returns {AsyncGenerator<T, void, unknown>}
     */
    async *listen(collection) {
        const target = this.journalPath(collection)
        let position = 0
        while (true) {
            try {
                const fileStat = await stat(target)
                if (fileStat.size > position) {
                    const handle = await open(target, 'r')
                    try {
                        const size = fileStat.size - position
                        const buffer = Buffer.alloc(size)
                        await handle.read(buffer, 0, size, position)
                        position = fileStat.size
                        for (const line of buffer.toString('utf8').split('\n')) {
                            if (line.trim().length === 0) continue
                            yield JSON.parse(line)
                        }
                    } finally {
                        await handle.close()
                    }
                }
            } catch (err) {
                const error = /** @type {NodeJS.ErrnoException} */ (err)
                if (error.code !== 'ENOENT') throw err
            }
            await Bun.sleep(100)
        }
    }
}
