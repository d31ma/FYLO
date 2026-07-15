import { constants } from 'node:fs'
import { chmod, lstat, mkdir, open, readdir, rename, rm, stat, utimes } from 'node:fs/promises'
import path from 'node:path'
import { assertPathInside, validateDocId } from '../core/doc-id.js'
import { writeDurable, writeDurableStream } from './durable.js'
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
        const handle = await open(target, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0))
        try {
            return await handle.readFile('utf8')
        } finally {
            await handle.close()
        }
    }
    /**
     * @param {string} target
     * @returns {Promise<Uint8Array>}
     */
    async readBytes(target) {
        const handle = await open(target, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0))
        try {
            return new Uint8Array(await handle.readFile())
        } finally {
            await handle.close()
        }
    }
    /**
     * @param {string} target
     * @param {{ start?: number, end?: number }} [range] half-open byte range [start, end)
     * @returns {ReadableStream<Uint8Array>}
     */
    readStream(target, range) {
        if (range?.end !== undefined && range.end <= (range.start ?? 0)) {
            return new ReadableStream({
                start(controller) {
                    controller.close()
                }
            })
        }
        /** @type {import('node:fs').ReadStream | undefined} */
        let stream
        return new ReadableStream({
            async start(controller) {
                try {
                    const handle = await open(
                        target,
                        constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0)
                    )
                    stream = handle.createReadStream({
                        autoClose: true,
                        ...(range?.start !== undefined ? { start: range.start } : {}),
                        ...(range?.end !== undefined ? { end: range.end - 1 } : {})
                    })
                    for await (const chunk of stream) {
                        controller.enqueue(new Uint8Array(/** @type {Buffer} */ (chunk)))
                    }
                    controller.close()
                } catch (error) {
                    controller.error(error)
                }
            },
            cancel() {
                stream?.destroy()
            }
        })
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
     * @param {ReadableStream<Uint8Array>} stream
     * @param {{ maxBytes?: number }} [options]
     * @returns {Promise<{ contentLength: number, checksumSHA256: string }>}
     */
    async writeStream(target, stream, options) {
        return await writeDurableStream(target, stream, options)
    }
    /**
     * @param {string} source
     * @param {string} target
     * @returns {Promise<void>}
     */
    async move(source, target) {
        await mkdir(path.dirname(target), { recursive: true })
        await rename(source, target)
    }
    /**
     * @param {string} target
     * @param {number} mode
     * @returns {Promise<void>}
     */
    async chmod(target, mode) {
        await chmod(target, mode)
    }
    /**
     * @param {string} target
     * @param {number} mtimeMs
     * @returns {Promise<void>}
     */
    async setModifiedTime(target, mtimeMs) {
        const modified = new Date(mtimeMs)
        await utimes(target, modified, modified)
    }
    /**
     * @param {string} target
     * @returns {Promise<{ mtimeMs: number, size: number }>}
     */
    async metadata(target) {
        const metadata = await lstat(target)
        if (metadata.isSymbolicLink() || !metadata.isFile()) {
            throw new Error(`Storage target must be a regular, non-link file: ${target}`)
        }
        return { mtimeMs: metadata.mtimeMs, size: metadata.size }
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
            await lstat(target)
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
    /** @type {(collection: string) => string} */
    collectionRoot
    /** @type {StorageEngine} */
    storage
    /**
     * @param {(collection: string) => string} collectionRoot Resolves a
     *   collection's on-disk root (kind-aware: `.collections` or `.buckets`).
     * @param {StorageEngine} storage
     */
    constructor(collectionRoot, storage) {
        this.collectionRoot = collectionRoot
        this.storage = storage
    }
    /**
     * @param {string} collection
     * @param {TTIDValue} docId
     * @returns {Promise<string>}
     */
    async lockPath(collection, docId) {
        await validateDocId(docId)
        const locksRoot = path.join(this.collectionRoot(collection), 'locks')
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
        return await tryAcquireFileLock(
            await this.lockPath(collection, docId),
            owner,
            ttlMsOrOptions
        )
    }
    /**
     * Releases a lock only when the current owner matches.
     * @param {string} collection
     * @param {TTIDValue} docId
     * @param {string} owner
     * @returns {Promise<void>}
     */
    async release(collection, docId, owner) {
        await tryReleaseFileLock(await this.lockPath(collection, docId), owner)
    }
    /**
     * @param {string} collection
     * @returns {string}
     */
    collectionLockPath(collection) {
        return path.join(this.collectionRoot(collection), 'locks', 'collection.lock')
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
    /** @type {(collection: string) => string} */
    collectionRoot
    /** @type {StorageEngine} */
    storage
    /**
     * @param {(collection: string) => string} collectionRoot Resolves a
     *   collection's on-disk root (kind-aware: `.collections` or `.buckets`).
     * @param {StorageEngine} storage
     */
    constructor(collectionRoot, storage) {
        this.collectionRoot = collectionRoot
        this.storage = storage
    }
    /**
     * @param {string} collection
     * @returns {string}
     */
    journalPath(collection) {
        return path.join(this.collectionRoot(collection), 'events', `${collection}.ndjson`)
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

    /**
     * Reads the events appended since byte offset `fromOffset`. Only whole lines
     * are consumed, so a torn final write is left for the next read. Returns the
     * new offset (a line boundary) for the caller to resume from.
     * @template {Record<string, any>} T
     * @param {string} collection
     * @param {number} [fromOffset]
     * @returns {Promise<{ events: T[], offset: number }>}
     */
    async readSince(collection, fromOffset = 0) {
        const target = this.journalPath(collection)
        try {
            const fileStat = await stat(target)
            if (fileStat.size <= fromOffset) return { events: [], offset: fileStat.size }
            const handle = await open(target, 'r')
            try {
                const size = fileStat.size - fromOffset
                const buffer = Buffer.alloc(size)
                await handle.read(buffer, 0, size, fromOffset)
                const text = buffer.toString('utf8')
                const lastNewline = text.lastIndexOf('\n')
                if (lastNewline === -1) return { events: [], offset: fromOffset }
                const complete = text.slice(0, lastNewline + 1)
                /** @type {T[]} */
                const events = []
                for (const line of complete.split('\n')) {
                    if (line.trim().length === 0) continue
                    events.push(JSON.parse(line))
                }
                return { events, offset: fromOffset + Buffer.byteLength(complete, 'utf8') }
            } finally {
                await handle.close()
            }
        } catch (err) {
            const error = /** @type {NodeJS.ErrnoException} */ (err)
            if (error.code !== 'ENOENT') throw err
            return { events: [], offset: 0 }
        }
    }

    /**
     * Current byte length of the collection journal — the offset a fresh client
     * should resume streaming from. Cheap (stat only).
     * @param {string} collection
     * @returns {Promise<number>}
     */
    async currentOffset(collection) {
        try {
            return (await stat(this.journalPath(collection))).size
        } catch (err) {
            const error = /** @type {NodeJS.ErrnoException} */ (err)
            if (error.code !== 'ENOENT') throw err
            return 0
        }
    }

    /**
     * Tails the collection journal from byte offset `fromOffset`, yielding one
     * `{ events, offset }` batch per poll that has new events. Stops when
     * `signal` aborts.
     * @template {Record<string, any>} T
     * @param {string} collection
     * @param {number} [fromOffset]
     * @param {AbortSignal} [signal]
     * @returns {AsyncGenerator<{ events: T[], offset: number }, void, unknown>}
     */
    async *tailFrom(collection, fromOffset = 0, signal) {
        let offset = fromOffset
        while (!signal?.aborted) {
            const batch = await this.readSince(collection, offset)
            if (batch.events.length > 0 || batch.offset !== offset) {
                offset = batch.offset
                yield /** @type {{ events: T[], offset: number }} */ (batch)
            }
            await Bun.sleep(100)
        }
    }
}
