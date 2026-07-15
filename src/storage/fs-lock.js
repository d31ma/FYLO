/**
 * Advisory file-based locking for FYLO collection and document writes.
 *
 * Correctness model: read this before changing anything in here.
 *
 * This module implements *advisory* locking on top of `node:fs/promises`
 * primitives (`link`, `unlink`, `rename`). It is not built on POSIX
 * `fcntl(F_SETLK)` or `flock`; those would give true OS-level mutual
 * exclusion, but they are not available through `node:fs/promises` and
 * they do not survive across `bun build --compile` targets uniformly.
 *
 * Without OS-level compare-and-swap, the takeover and heartbeat-refresh
 * paths each contain a small residual race window: between any
 * `readLockMeta(...)` (or other observation) and the subsequent `unlink`
 * or `rename`, a different process can take over the lock and have its
 * fresh write clobbered.
 *
 * The mitigations layered here:
 * - `link()`-based atomic create on the happy path (only one acquirer
 *   can win a contested fresh acquire).
 * - 5-minute TTL on collection writes, with a heartbeat refresh every
 *   `ttlMs/3`. The heartbeat keeps the trigger for takeover from
 *   firing during normal operation, so the race window only opens when
 *   a holder is genuinely dead (crashed) or paused for >5 minutes.
 * - Re-validation of the stale metadata immediately before the takeover
 *   `unlink`. This narrows the cross-process window to microseconds.
 * - Heartbeat ticks re-check ownership and the cancellation flag right
 *   before `rename`, and `tryReleaseFileLock` drains in-flight ticks
 *   before unlinking so a stale tick cannot resurrect a released lock.
 *
 * For workloads requiring strictly strong cross-process exclusion under
 * adversarial timing, consider switching this primitive to `fcntl`
 * locking or a central coordinator (Redis/ZooKeeper). That is a 3.x
 * follow-up; the 3.0 release ships with the advisory model documented
 * here.
 */

import { link, mkdir, readFile, rename, unlink, writeFile } from 'node:fs/promises'
import path from 'node:path'

/**
 * @typedef {object} HeartbeatEntry
 * @property {ReturnType<typeof setInterval>} interval
 * @property {string} owner
 * @property {boolean} cancelled
 * @property {Promise<void> | null} inFlight
 */

/** @type {Map<string, HeartbeatEntry>} */
const heartbeats = new Map()

/**
 * @param {string} lockPath
 * @param {string} suffix
 * @returns {string}
 */
function uniqueLockScratchPath(lockPath, suffix) {
    const nonce = Math.random().toString(36).slice(2)
    return `${lockPath}.${process.pid}.${Date.now()}.${nonce}.${suffix}`
}

/**
 * Tracks lock heartbeat timers so long-running lock holders can refresh TTLs
 * and release can drain in-flight refreshes safely.
 */
class FileLockHeartbeatRegistry {
    /** @param {Map<string, HeartbeatEntry>} entries */
    constructor(entries = new Map()) {
        this.entries = entries
    }

    /**
     * @param {string} lockPath
     * @param {string} owner
     * @param {number} ttlMs
     */
    start(lockPath, owner, ttlMs) {
        void this.stop(lockPath)
        const intervalMs = Math.max(Math.floor(ttlMs / 3), 100)
        /** @type {HeartbeatEntry} */
        const entry = {
            interval: /** @type {any} */ (null),
            owner,
            cancelled: false,
            inFlight: null
        }
        const tick = async () => {
            await this.#tick(lockPath, entry)
        }
        entry.interval = setInterval(() => {
            if (entry.cancelled || entry.inFlight) return
            entry.inFlight = tick().finally(() => {
                entry.inFlight = null
            })
        }, intervalMs)
        if (typeof entry.interval.unref === 'function') entry.interval.unref()
        this.entries.set(lockPath, entry)
    }

    /**
     * @param {string} lockPath
     * @param {HeartbeatEntry} entry
     */
    async #tick(lockPath, entry) {
        if (entry.cancelled) return
        const scratchPath = uniqueLockScratchPath(lockPath, 'heartbeat.tmp')
        try {
            const meta = await readLockMeta(lockPath)
            if (entry.cancelled || !meta || meta.owner !== entry.owner) return
            await writeFile(
                scratchPath,
                JSON.stringify({ owner: entry.owner, pid: process.pid, ts: Date.now() })
            )
            if (entry.cancelled) return
            const refreshedMeta = await readLockMeta(lockPath)
            if (entry.cancelled || !refreshedMeta || refreshedMeta.owner !== entry.owner) return
            await rename(scratchPath, lockPath)
        } catch {
            // Heartbeat loss is recovered by the TTL takeover path.
        } finally {
            try {
                await unlink(scratchPath)
            } catch (err) {
                const error = /** @type {NodeJS.ErrnoException} */ (err)
                if (error && error.code !== 'ENOENT') throw err
            }
        }
    }

    /**
     * @param {string} lockPath
     * @returns {Promise<void>}
     */
    async stop(lockPath) {
        const entry = this.entries.get(lockPath)
        if (!entry) return
        entry.cancelled = true
        clearInterval(entry.interval)
        this.entries.delete(lockPath)
        if (entry.inFlight) {
            try {
                await entry.inFlight
            } catch {
                // The heartbeat path is best effort; release should still continue.
            }
        }
    }
}

const heartbeatRegistry = new FileLockHeartbeatRegistry(heartbeats)

/**
 * @param {string} lockPath
 * @param {string} owner
 * @param {number} ttlMs
 */
function startHeartbeat(lockPath, owner, ttlMs) {
    heartbeatRegistry.start(lockPath, owner, ttlMs)
}

/**
 * Cancels and drains the heartbeat for `lockPath`. Awaits any tick that
 * is already in flight so callers can safely `unlink` the lock file
 * afterwards without racing with a stale rename.
 *
 * @param {string} lockPath
 * @returns {Promise<void>}
 */
async function stopHeartbeat(lockPath) {
    await heartbeatRegistry.stop(lockPath)
}

/**
 * Reads and parses a lock file's JSON payload.
 * Returns null if the file is missing or unreadable; returns the parsed
 * object otherwise. Corrupt JSON yields null (treated as stale).
 *
 * @param {string} lockPath
 * @returns {Promise<{ owner: string, pid?: number, ts: number } | null>}
 */
async function readLockMeta(lockPath) {
    try {
        const raw = await Bun.file(lockPath).text()
        return JSON.parse(raw)
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code === 'ENOENT') return null
        return null
    }
}

/** @param {string} lockPath @returns {Promise<string | null>} */
async function readLockPayload(lockPath) {
    try {
        return await readFile(lockPath, 'utf8')
    } catch {
        return null
    }
}

/**
 * A lock left by a process that no longer exists is immediately reclaimable;
 * startup recovery must not wait for a multi-minute TTL after SIGKILL. Locks
 * created by older FYLO versions have no pid and retain TTL-only semantics.
 * @param {{ pid?: number } | null} meta
 * @returns {boolean}
 */
function lockOwnerIsAlive(meta) {
    if (!meta || !Number.isSafeInteger(meta.pid) || /** @type {number} */ (meta.pid) <= 0) {
        return true
    }
    try {
        process.kill(/** @type {number} */ (meta.pid), 0)
        return true
    } catch (error) {
        return /** @type {NodeJS.ErrnoException} */ (error).code !== 'ESRCH'
    }
}

/**
 * Attempts an atomic create-exclusive of the lock file with the given payload.
 * Uses `link()` rather than `open(wx)` so the file appears at `lockPath` with
 * its content already populated; this closes the race where a concurrent
 * reader sees an empty file after `open(wx)` but before `write`.
 *
 * Resolves true on success; resolves false if the target already existed.
 * Any other error rethrows.
 *
 * @param {string} lockPath
 * @param {string} payload
 * @returns {Promise<boolean>}
 */
async function tryCreateExclusive(lockPath, payload) {
    const scratchPath = uniqueLockScratchPath(lockPath, 'tmp')
    await writeFile(scratchPath, payload)
    try {
        await link(scratchPath, lockPath)
        return true
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code === 'EEXIST') return false
        throw err
    } finally {
        try {
            await unlink(scratchPath)
        } catch (cleanupErr) {
            const cleanupError = /** @type {NodeJS.ErrnoException} */ (cleanupErr)
            if (cleanupError.code !== 'ENOENT') throw cleanupErr
        }
    }
}

/**
 * @typedef {object} TryAcquireFileLockOptions
 * @property {number=} ttlMs
 * @property {(info: { lockPath: string, newOwner: string, previousOwner?: string }) => void=} onTakeover
 *   Invoked after a stale lock is successfully reclaimed. Not called for
 *   live-lock rejections or lost takeover races.
 * @property {boolean=} heartbeat
 *   When true, refresh the lock's timestamp every `ttlMs/3` while held so
 *   long-running operations are not misclassified as stale. Stopped by
 *   `tryReleaseFileLock`. Off by default; enable for collection-scope
 *   locks held for the duration of bulk writes or rebuilds.
 */

/**
 * Acquires an advisory file-based lock.
 *
 * Semantics:
 * - Atomic `link()` is the only path by which ownership is established; the
 *   filesystem guarantees at most one concurrent acquirer wins.
 * - If the lock already exists and its timestamp is within `ttlMs`, the
 *   current holder is considered live and this call returns false.
 * - If the lock is stale (or its payload is missing/corrupt), a single
 *   takeover attempt is made: unlink + retry `wx`. The loser of a
 *   concurrent takeover race returns false and should retry at the
 *   caller layer.
 *
 * Release is only safe if the holder completes their work before `ttlMs`
 * elapses. Callers should choose a TTL comfortably larger than their
 * longest expected operation.
 *
 * @param {string} lockPath
 * @param {string} owner
 * @param {number | TryAcquireFileLockOptions} [ttlMsOrOptions]
 * @returns {Promise<boolean>}
 */
export async function tryAcquireFileLock(lockPath, owner, ttlMsOrOptions = 30_000) {
    const options = typeof ttlMsOrOptions === 'number' ? { ttlMs: ttlMsOrOptions } : ttlMsOrOptions
    const ttlMs = options.ttlMs ?? 30_000
    await mkdir(path.dirname(lockPath), { recursive: true })
    const payload = JSON.stringify({ owner, pid: process.pid, ts: Date.now() })
    if (await tryCreateExclusive(lockPath, payload)) {
        if (options.heartbeat) startHeartbeat(lockPath, owner, ttlMs)
        return true
    }
    const meta = await readLockMeta(lockPath)
    if (
        meta &&
        typeof meta.ts === 'number' &&
        Date.now() - meta.ts <= ttlMs &&
        lockOwnerIsAlive(meta)
    ) {
        return false
    }
    const previousOwner = meta && typeof meta.owner === 'string' ? meta.owner : undefined
    if (meta) {
        const metaCheck = await readLockMeta(lockPath)
        // A missing second observation means another contender already moved
        // or removed the stale lock. Do not unlink in that case: it may have
        // created a fresh lock between our read and this takeover attempt.
        if (!metaCheck) return false
        if (metaCheck.owner !== meta.owner || metaCheck.ts !== meta.ts) return false
    } else {
        // Preserve recovery of genuinely corrupt lock files without treating a
        // transiently missing/replaced lock as corrupt. Two identical raw reads
        // must still be invalid JSON before this contender may remove it.
        const firstPayload = await readLockPayload(lockPath)
        const secondPayload = await readLockPayload(lockPath)
        if (firstPayload === null || secondPayload === null || firstPayload !== secondPayload) {
            return false
        }
        try {
            JSON.parse(secondPayload)
            return false
        } catch {
            // Stable invalid payload: reclaim below.
        }
    }
    try {
        await unlink(lockPath)
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code !== 'ENOENT') throw err
    }
    const acquired = await tryCreateExclusive(lockPath, payload)
    if (acquired) {
        if (options.onTakeover) {
            try {
                options.onTakeover({ lockPath, newOwner: owner, previousOwner })
            } catch (err) {
                console.error('FYLO onTakeover callback threw:', err)
            }
        }
        if (options.heartbeat) startHeartbeat(lockPath, owner, ttlMs)
    }
    return acquired
}

/**
 * Blocking variant of `tryAcquireFileLock`: polls with exponential backoff
 * (capped) until the lock is acquired or `waitTimeoutMs` elapses. Throws
 * on timeout. `ttlMs` controls stale-lock takeover; see `tryAcquireFileLock`.
 *
 * @param {string} lockPath
 * @param {string} owner
 * @param {object} [options]
 * @param {number} [options.ttlMs]
 * @param {number} [options.waitTimeoutMs]
 * @param {boolean} [options.heartbeat]
 * @param {(info: { lockPath: string, newOwner: string, previousOwner?: string }) => void} [options.onTakeover]
 * @returns {Promise<void>}
 */
export async function waitAcquireFileLock(lockPath, owner, options = {}) {
    const ttlMs = options.ttlMs ?? 30_000
    const waitTimeoutMs = options.waitTimeoutMs ?? 60_000
    const onTakeover = options.onTakeover
    const heartbeat = options.heartbeat ?? false
    const deadline = Date.now() + waitTimeoutMs
    let delay = 2
    while (true) {
        if (await tryAcquireFileLock(lockPath, owner, { ttlMs, onTakeover, heartbeat })) return
        if (Date.now() >= deadline) {
            throw new Error(`Timed out waiting for filesystem lock at ${lockPath}`)
        }
        await Bun.sleep(delay)
        delay = Math.min(delay * 2, 100)
    }
}

/**
 * Releases the lock at `lockPath` if (and only if) the current payload
 * names this owner. Missing lock files are silently ignored.
 *
 * Note: release is not atomic with the ownership check. A concurrent
 * stale-lock takeover between our read and unlink could cause us to
 * delete someone else's lock. This is acceptable for FYLO's advisory
 * locking: callers rely on short operation durations plus TTL-based
 * correctness, not fine-grained release ordering.
 *
 * @param {string} lockPath
 * @param {string} owner
 * @returns {Promise<void>}
 */
export async function tryReleaseFileLock(lockPath, owner) {
    await stopHeartbeat(lockPath)
    const meta = await readLockMeta(lockPath)
    if (!meta || meta.owner !== owner) return
    try {
        await unlink(lockPath)
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code !== 'ENOENT') throw err
    }
}
