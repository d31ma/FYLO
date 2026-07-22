/**
 * Advisory file-based locking for FYLO collection and document writes.
 *
 * Correctness model: read this before changing anything in here.
 *
 * This module implements *advisory* ownership with `node:fs/promises`
 * primitives (`link`, `unlink`, `rename`). Stale takeover alone is serialized
 * by a kernel `flock` sentinel loaded through Bun FFI on supported POSIX
 * targets; unsupported targets fail closed instead of risking two winners.
 *
 * Locks created by the current release carry a local process id. A live
 * process is never evicted merely because a wall-clock TTL elapsed; this
 * removes the unsafe live-owner takeover window. TTL takeover remains only
 * for legacy lock files that have no process identity.
 *
 * The mitigations layered here:
 * - `link()`-based atomic create on the happy path (only one acquirer
 *   can win a contested fresh acquire).
 * - 5-minute TTL on legacy collection locks, with a heartbeat refresh every
 *   `ttlMs/3` for compatibility with older FYLO processes.
 * - Re-validation of stale metadata followed by an atomic rename into a
 *   unique quarantine path before deletion. A contender never unlinks a
 *   newly replaced lock at the canonical path.
 * - A persistent kernel-locked takeover sentinel. The file may outlive a
 *   process, but its ownership cannot: the kernel drops `flock` on exit or
 *   SIGKILL, so dead contenders never strand future recovery.
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
import { tryAcquireProcessFileLock } from './secure-open.js'

/**
 * @typedef {object} HeartbeatEntry
 * @property {ReturnType<typeof setInterval>} interval
 * @property {string} owner
 * @property {string | undefined} processIdentity
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
     * @param {string | undefined} processIdentity
     * @param {number} ttlMs
     */
    start(lockPath, owner, processIdentity, ttlMs) {
        void this.stop(lockPath)
        const intervalMs = Math.max(Math.floor(ttlMs / 3), 100)
        /** @type {HeartbeatEntry} */
        const entry = {
            interval: /** @type {any} */ (null),
            owner,
            processIdentity,
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
                JSON.stringify({
                    owner: entry.owner,
                    pid: process.pid,
                    processIdentity: entry.processIdentity,
                    ts: Date.now()
                })
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
 * @param {string | undefined} processIdentity
 * @param {number} ttlMs
 */
function startHeartbeat(lockPath, owner, processIdentity, ttlMs) {
    heartbeatRegistry.start(lockPath, owner, processIdentity, ttlMs)
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
 * @returns {Promise<{ owner: string, pid?: number, processIdentity?: string, ts: number } | null>}
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
 * @param {{ pid?: number, processIdentity?: string } | null} meta
 * @returns {Promise<boolean>}
 */
async function lockOwnerIsAlive(meta) {
    if (!meta || !Number.isSafeInteger(meta.pid) || /** @type {number} */ (meta.pid) <= 0) {
        return true
    }
    try {
        process.kill(/** @type {number} */ (meta.pid), 0)
    } catch (error) {
        return /** @type {NodeJS.ErrnoException} */ (error).code !== 'ESRCH'
    }
    if (typeof meta.processIdentity !== 'string') return true
    const observedIdentity = await processIdentity(/** @type {number} */ (meta.pid))
    return observedIdentity === null || observedIdentity === meta.processIdentity
}

/**
 * Returns an OS-issued process incarnation identifier. A PID alone is not an
 * identity because kernels reuse it after exit (and across restarts).
 *
 * @param {number} pid
 * @returns {Promise<string | null>}
 */
async function processIdentity(pid) {
    try {
        if (process.platform === 'linux') {
            const [bootId, statLine] = await Promise.all([
                readFile('/proc/sys/kernel/random/boot_id', 'utf8'),
                readFile(`/proc/${pid}/stat`, 'utf8')
            ])
            const fields = statLine
                .slice(statLine.lastIndexOf(')') + 2)
                .trim()
                .split(/\s+/)
            const startTicks = fields[19]
            if (!startTicks) return null
            return `linux:${bootId.trim()}:${startTicks}`
        }

        const command =
            process.platform === 'win32'
                ? [
                      'powershell.exe',
                      '-NoProfile',
                      '-NonInteractive',
                      '-Command',
                      `(Get-CimInstance Win32_Process -Filter 'ProcessId = ${pid}').CreationDate.ToUniversalTime().Ticks`
                  ]
                : ['ps', '-o', 'lstart=', '-p', String(pid)]
        const child = Bun.spawn(command, { stdout: 'pipe', stderr: 'ignore' })
        const output = (await new Response(child.stdout).text()).trim()
        if ((await child.exited) !== 0 || output.length === 0) return null
        return `${process.platform}:${output}`
    } catch {
        return null
    }
}

/** @type {Promise<string | null> | undefined} */
let ownProcessIdentity

/** @returns {Promise<string | undefined>} */
async function getOwnProcessIdentity() {
    if (!ownProcessIdentity) ownProcessIdentity = processIdentity(process.pid)
    return (await ownProcessIdentity) ?? undefined
}

/** @param {{ pid?: number } | null} meta */
function hasProcessIdentity(meta) {
    return Boolean(meta && Number.isSafeInteger(meta.pid) && /** @type {number} */ (meta.pid) > 0)
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
    const identity = await getOwnProcessIdentity()
    const payload = JSON.stringify({
        owner,
        pid: process.pid,
        processIdentity: identity,
        ts: Date.now()
    })
    if (await tryCreateExclusive(lockPath, payload)) {
        if (options.heartbeat) startHeartbeat(lockPath, owner, identity, ttlMs)
        return true
    }
    const meta = await readLockMeta(lockPath)
    if (hasProcessIdentity(meta) && (await lockOwnerIsAlive(meta))) return false
    if (
        meta &&
        !hasProcessIdentity(meta) &&
        typeof meta.ts === 'number' &&
        Date.now() - meta.ts <= ttlMs
    )
        return false
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
    // Serialize takeover independently of the stale lock path. Without this
    // claim, two contenders can both rename successive generations and both
    // believe they acquired the lock.
    const takeoverClaim = `${lockPath}.takeover`
    const releaseTakeoverClaim = tryAcquireProcessFileLock(takeoverClaim)
    if (!releaseTakeoverClaim) return false
    const quarantine = uniqueLockScratchPath(lockPath, 'stale')
    try {
        // The lock may have changed while this contender was claiming the
        // takeover lane. Revalidate it under the claim before moving it.
        const currentPayload = await readLockPayload(lockPath)
        const observedPayload = meta ? JSON.stringify(meta) : await readLockPayload(lockPath)
        if (currentPayload === null || observedPayload === null) return false
        let currentMeta
        try {
            currentMeta = JSON.parse(currentPayload)
        } catch {
            currentMeta = null
        }
        if (meta) {
            if (
                !currentMeta ||
                currentMeta.owner !== meta.owner ||
                currentMeta.pid !== meta.pid ||
                currentMeta.processIdentity !== meta.processIdentity ||
                currentMeta.ts !== meta.ts
            )
                return false
        } else if (currentPayload !== observedPayload) {
            return false
        }
        await rename(lockPath, quarantine)
        await unlink(quarantine)
        const acquired = await tryCreateExclusive(lockPath, payload)
        if (!acquired) return false
        if (options.onTakeover) {
            try {
                options.onTakeover({ lockPath, newOwner: owner, previousOwner })
            } catch (err) {
                console.error('FYLO onTakeover callback threw:', err)
            }
        }
        if (options.heartbeat) startHeartbeat(lockPath, owner, identity, ttlMs)
        return true
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code === 'ENOENT') return false
        throw err
    } finally {
        releaseTakeoverClaim()
    }
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
 * Current locks cannot be taken over while their pid is alive. Release moves
 * the owned entry out of the lock namespace atomically before deleting it, so
 * a waiting contender can only create a new generation after that move.
 *
 * @param {string} lockPath
 * @param {string} owner
 * @returns {Promise<void>}
 */
export async function tryReleaseFileLock(lockPath, owner) {
    await stopHeartbeat(lockPath)
    const meta = await readLockMeta(lockPath)
    if (!meta || meta.owner !== owner) return
    const released = uniqueLockScratchPath(lockPath, 'released')
    try {
        await rename(lockPath, released)
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code === 'ENOENT') return
        throw err
    }
    try {
        await unlink(released)
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code !== 'ENOENT') throw err
    }
}
