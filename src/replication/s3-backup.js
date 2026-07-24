/**
 * Whole-root S3 backup. The local filesystem is always the source of truth;
 * this mirrors the entire FYLO root (documents, buckets, index, catalog, vcs)
 * to a single S3 bucket. Two modes work together:
 *   - mirror-on-write: {@link FyloS3Backup.mirror}/{@link FyloS3Backup.remove}
 *     push the files an operation touched, for freshness;
 *   - reconcile: {@link FyloS3Backup.reconcile} walks the whole root and makes
 *     S3 match it exactly (upload changed, delete removed) — the correctness
 *     backstop, runnable on demand or on an interval.
 *
 * Object keys are the file's path relative to the root (posix separators),
 * optionally under a key prefix.
 */

import path from 'node:path'
import { createHash } from 'node:crypto'
import { readdir, stat } from 'node:fs/promises'
import { isDurableWriteScratchPath } from '../storage/durable.js'
import { getXattrFd, listXattrFd } from '../storage/xattr.js'
import {
    closeSecureDescriptor,
    openDirectoryNoFollow,
    openFileAtRoot,
    openFileAtRootStrict,
    readAllSecureDescriptor,
    readSecureDescriptor,
    statSecureDescriptor
} from '../storage/secure-open.js'
import { emitFyloEvent } from '../observability/events.js'
import { CoalescingScheduler } from './coalescing-scheduler.js'

const BACKUP_METADATA_DIR = '.fylo-backup/xattrs/'
const LOCAL_TRANSACTION_DIR = '.fylo-transactions'
const DEFAULT_MAX_FILE_BYTES = 64 * 1024 * 1024
const DEFAULT_MAX_MANIFEST_BYTES = 1024 * 1024
const DEFAULT_MAX_RECONCILE_SNAPSHOT_BYTES = 512 * 1024 * 1024
const MAX_GENERATION_STATE_BYTES = 16 * 1024
const RETRYABLE_CODES = new Set([
    'ECONNRESET',
    'ECONNREFUSED',
    'EHOSTUNREACH',
    'ENETUNREACH',
    'ETIMEDOUT',
    'EAI_AGAIN',
    'SlowDown',
    'RequestTimeout',
    'Throttling',
    'ThrottlingException'
])

/** @param {unknown} error @returns {boolean} */
function isRetryable(error) {
    const candidate = /** @type {{ code?: string, status?: number, statusCode?: number }} */ (error)
    const status = candidate?.status ?? candidate?.statusCode
    return (
        (typeof status === 'number' && (status === 408 || status === 429 || status >= 500)) ||
        (typeof candidate?.code === 'string' && RETRYABLE_CODES.has(candidate.code))
    )
}

/** @param {number} value @param {string} name @param {number} minimum */
function positiveInteger(value, name, minimum = 1) {
    if (!Number.isSafeInteger(value) || value < minimum) {
        throw new TypeError(`sync.s3.${name} must be a safe integer >= ${minimum}`)
    }
}

/**
 * Bounded parallel mapper that stops scheduling after the first failure.
 * @template T
 * @param {T[]} values
 * @param {number} concurrency
 * @param {(value: T) => Promise<void>} operation
 */
async function mapLimit(values, concurrency, operation) {
    let cursor = 0
    /** @type {unknown} */
    let failure
    const workers = Array.from({ length: Math.min(concurrency, values.length) }, async () => {
        while (failure === undefined) {
            const index = cursor++
            if (index >= values.length) return
            try {
                await operation(values[index])
            } catch (error) {
                failure = error
            }
        }
    })
    await Promise.all(workers)
    if (failure !== undefined) throw failure
}

/** @param {Uint8Array} bytes @returns {string} */
function sha256(bytes) {
    return createHash('sha256').update(bytes).digest('hex')
}

export function backupPlatform() {
    return process.platform === 'win32' ? 'windows-ntfs' : 'posix'
}

/**
 * @param {string} dataKey
 * @param {Uint8Array} bytes
 * @param {Record<string, string>} xattrs
 * @param {{ mode: number, mtimeMs: number }} native
 */
function fileManifest(dataKey, bytes, xattrs, native) {
    return JSON.stringify({
        version: 2,
        platform: backupPlatform(),
        dataKey,
        size: bytes.byteLength,
        sha256: sha256(bytes),
        xattrs,
        native
    })
}

/**
 * Read a stream into one fixed-capacity allocation. The stream is cancelled as
 * soon as the next chunk would cross the limit, so an incorrect or absent S3
 * listing size cannot turn a manifest read into an unbounded allocation.
 *
 * @param {ReadableStream<Uint8Array>} stream
 * @param {number} maximum
 * @param {() => RangeError} overflowError
 * @returns {Promise<Uint8Array>}
 */
async function readBounded(stream, maximum, overflowError) {
    const bytes = new Uint8Array(maximum)
    const reader = stream.getReader()
    let length = 0
    try {
        while (true) {
            const { done, value } = await reader.read()
            if (done) return bytes.subarray(0, length)
            const chunk = value instanceof Uint8Array ? value : new Uint8Array(value)
            if (chunk.byteLength > maximum - length) {
                await reader.cancel()
                throw overflowError()
            }
            bytes.set(chunk, length)
            length += chunk.byteLength
        }
    } finally {
        reader.releaseLock()
    }
}

/**
 * @typedef {object} FyloS3BackupOptions
 * @property {string} bucket
 * @property {string=} prefix
 * @property {boolean=} allowBucketRoot explicitly permit destructive bucket-root reconciliation
 * @property {number=} reconcileIntervalMs
 * @property {number=} concurrency maximum simultaneous S3 requests (default 4)
 * @property {number=} maxFileBytes reject files larger than this before reading (default 64 MiB)
 * @property {number=} maxManifestBytes reject remote manifests larger than this (default 1 MiB)
 * @property {number=} maxReconcileSnapshotBytes cap immutable reconcile materialization (default 512 MiB)
 * @property {{ attempts?: number, baseDelayMs?: number, maxDelayMs?: number }=} retry
 * @property {string=} accessKeyId
 * @property {string=} secretAccessKey
 * @property {string=} sessionToken
 * @property {string=} endpoint
 * @property {string=} region
 */

/**
 * First defined environment variable from `names`, else undefined.
 * @param {string[]} names
 * @returns {string | undefined}
 */
function envValue(names) {
    for (const name of names) {
        const value = process.env[name]
        if (value) return value
    }
    return undefined
}

/**
 * Fill missing S3 credentials from the standard AWS / FYLO env vars.
 * @param {FyloS3BackupOptions} options
 * @returns {FyloS3BackupOptions}
 */
export function resolveS3BackupOptions(options) {
    return {
        ...options,
        accessKeyId:
            options.accessKeyId ?? envValue(['AWS_ACCESS_KEY_ID', 'FYLO_S3_ACCESS_KEY_ID']),
        secretAccessKey:
            options.secretAccessKey ??
            envValue(['AWS_SECRET_ACCESS_KEY', 'FYLO_S3_SECRET_ACCESS_KEY']),
        sessionToken:
            options.sessionToken ?? envValue(['AWS_SESSION_TOKEN', 'FYLO_S3_SESSION_TOKEN']),
        endpoint:
            options.endpoint ??
            envValue(['AWS_ENDPOINT_URL_S3', 'AWS_ENDPOINT_URL', 'FYLO_S3_ENDPOINT']),
        region: options.region ?? envValue(['AWS_REGION', 'AWS_DEFAULT_REGION', 'FYLO_S3_REGION'])
    }
}

export class FyloS3Backup {
    /** @type {FyloS3BackupOptions} */
    options
    /** @type {string} */
    root
    /** @type {string} */
    prefix
    /** @type {Bun.S3Client} */
    client
    /** @type {CoalescingScheduler} */
    scheduler
    /** @type {Promise<void>} shared ordering lane for every remote mutation */
    mutationLane = Promise.resolve()
    activeRequests = 0
    /** @type {Array<() => void>} */
    requestWaiters = []
    /** @type {'open' | 'closing' | 'closed'} */
    state = 'open'
    /** @type {'manual' | 'scheduled'} */
    nextSource = 'manual'
    /** @type {AbortController} */
    abortController = new AbortController()
    /** @type {{ state: 'idle' | 'running' | 'failed' | 'closed', runs: number, lastStartedAt?: string, lastSuccessAt?: string, lastFailureAt?: string, lastError?: string }} */
    status = { state: 'idle', runs: 0 }
    /** @type {number | undefined} pinned root directory descriptor */
    rootFd

    /**
     * @param {FyloS3BackupOptions} options
     * @param {string} root absolute path of the FYLO root the S3 bucket mirrors
     * @param {{ client?: Bun.S3Client, onEvent?: import('../observability/events.js').FyloEventHandler, sleep?: (ms: number) => Promise<void>, random?: () => number }} [deps] injectable boundaries for tests
     */
    constructor(options, root, deps = {}) {
        this.options = resolveS3BackupOptions(options)
        this.onEvent = deps.onEvent
        this.sleep = deps.sleep ?? ((ms) => Bun.sleep(ms))
        this.random = deps.random ?? Math.random
        this.root = root
        if (!this.options.bucket || typeof this.options.bucket !== 'string') {
            throw new TypeError('sync.s3.bucket must be a non-empty string')
        }
        if (
            this.options.allowBucketRoot !== undefined &&
            typeof this.options.allowBucketRoot !== 'boolean'
        ) {
            throw new TypeError('sync.s3.allowBucketRoot must be a boolean')
        }
        if (this.options.prefix !== undefined && typeof this.options.prefix !== 'string') {
            throw new TypeError('sync.s3.prefix must be a string')
        }
        this.prefix = (this.options.prefix ?? '').replace(/^\/+|\/+$/g, '')
        if (
            this.prefix &&
            (this.prefix.includes('\\') ||
                this.prefix.split('/').some((part) => !part || part === '.' || part === '..') ||
                /[\0-\x1f\x7f]/.test(this.prefix))
        ) {
            throw new TypeError('sync.s3.prefix must be a normalized S3 key prefix')
        }
        if (!this.prefix && this.options.allowBucketRoot !== true) {
            throw new TypeError(
                'sync.s3.prefix must be non-empty; set allowBucketRoot: true only for a dedicated bucket'
            )
        }
        this.concurrency = this.options.concurrency ?? 4
        this.maxFileBytes = this.options.maxFileBytes ?? DEFAULT_MAX_FILE_BYTES
        this.maxManifestBytes = this.options.maxManifestBytes ?? DEFAULT_MAX_MANIFEST_BYTES
        this.maxReconcileSnapshotBytes =
            this.options.maxReconcileSnapshotBytes ?? DEFAULT_MAX_RECONCILE_SNAPSHOT_BYTES
        this.retry = {
            attempts: this.options.retry?.attempts ?? 3,
            baseDelayMs: this.options.retry?.baseDelayMs ?? 100,
            maxDelayMs: this.options.retry?.maxDelayMs ?? 5_000
        }
        positiveInteger(this.concurrency, 'concurrency')
        positiveInteger(this.maxFileBytes, 'maxFileBytes')
        positiveInteger(this.maxManifestBytes, 'maxManifestBytes')
        positiveInteger(this.maxReconcileSnapshotBytes, 'maxReconcileSnapshotBytes')
        positiveInteger(this.retry.attempts, 'retry.attempts')
        positiveInteger(this.retry.baseDelayMs, 'retry.baseDelayMs', 0)
        positiveInteger(this.retry.maxDelayMs, 'retry.maxDelayMs', 0)
        if (this.retry.maxDelayMs < this.retry.baseDelayMs) {
            throw new TypeError('sync.s3.retry.maxDelayMs must be >= retry.baseDelayMs')
        }
        this.client =
            deps.client ??
            new Bun.S3Client({
                accessKeyId: this.options.accessKeyId,
                secretAccessKey: this.options.secretAccessKey,
                sessionToken: this.options.sessionToken,
                endpoint: this.options.endpoint,
                region: this.options.region,
                bucket: this.options.bucket
            })
        this.scheduler = new CoalescingScheduler(() => this.reconcilePass(), {
            intervalMs: this.options.reconcileIntervalMs,
            beforeInterval: () => {
                this.nextSource = 'scheduled'
            },
            onError: () => {}
        })
    }

    /**
     * S3 object key for a local absolute path: its path relative to the root,
     * with posix separators, under the optional key prefix.
     * @param {string} absPath
     * @returns {string}
     */
    key(absPath) {
        const relative = path.relative(this.root, absPath).split(path.sep).join('/')
        if (
            !relative ||
            relative === '..' ||
            relative.startsWith('../') ||
            path.isAbsolute(relative)
        ) {
            throw new Error(`S3 backup path escapes the FYLO root: ${absPath}`)
        }
        return this.prefix ? `${this.prefix}/${relative}` : relative
    }

    /**
     * Read bytes and xattrs from one opened inode, rejecting symlinks and any
     * descriptor whose resolved target is outside the current FYLO root.
     * @param {string} target
     * @returns {Promise<{ bytes: Uint8Array, size: number, xattrs: Record<string, string>, native: { mode: number, mtimeMs: number } } | null>}
     */
    async snapshot(target) {
        let fd = null
        try {
            const rootFd = this.rootFd ?? openDirectoryNoFollow(this.root)
            this.rootFd = rootFd
            fd = openFileAtRoot(rootFd, path.relative(this.root, target))
            if (fd === null) return null
            const info = statSecureDescriptor(fd)
            if (!info.isFile()) return null
            if (info.size > this.maxFileBytes) {
                throw new RangeError(
                    `S3 backup file exceeds sync.s3.maxFileBytes (${info.size} > ${this.maxFileBytes}): ${target}`
                )
            }
            const bytes = new Uint8Array(readAllSecureDescriptor(fd, this.maxFileBytes))
            /** @type {Record<string, string>} */
            const xattrs = Object.create(null)
            for (const name of listXattrFd(fd).sort()) {
                const value = getXattrFd(fd, name)
                if (value !== null) xattrs[name] = Buffer.from(value).toString('base64')
            }
            return {
                bytes,
                size: info.size,
                xattrs,
                native: { mode: info.mode & 0o777, mtimeMs: info.mtimeMs }
            }
        } catch (error) {
            const code = /** @type {NodeJS.ErrnoException} */ (error).code
            if (code === 'ENOENT' || code === 'ELOOP' || code === 'ENOTDIR') return null
            throw error
        } finally {
            if (fd !== null) closeSecureDescriptor(fd)
        }
    }

    /** @param {string} dataKey @returns {string} */
    manifestKey(dataKey) {
        const base = this.prefix ? `${this.prefix}/` : ''
        return `${base}${BACKUP_METADATA_DIR}${Buffer.from(dataKey).toString('base64url')}.json`
    }

    /** @param {string} key @returns {boolean} */
    isManifestKey(key) {
        const base = this.prefix ? `${this.prefix}/` : ''
        return key.startsWith(`${base}${BACKUP_METADATA_DIR}`)
    }

    /** @param {string} key @param {number} [knownSize] @returns {Promise<Uint8Array>} */
    async readObject(key, knownSize = -1) {
        if (knownSize > this.maxManifestBytes) {
            throw new RangeError(
                `S3 backup manifest exceeds sync.s3.maxManifestBytes (${knownSize} > ${this.maxManifestBytes}): ${key}`
            )
        }
        const bytes = await this.withRetry('read', key, () =>
            readBounded(
                this.client.file(key).stream(),
                this.maxManifestBytes,
                () =>
                    new RangeError(
                        `S3 backup manifest exceeds sync.s3.maxManifestBytes (${this.maxManifestBytes} bytes): ${key}`
                    )
            )
        )
        return bytes
    }

    /** @param {string} operation @param {string} key @param {() => Promise<any>} task */
    async withRetry(operation, key, task) {
        for (let attempt = 1; ; attempt++) {
            if (this.abortController.signal.aborted) {
                throw new Error('S3 backup is closing')
            }
            try {
                return await this.runRequest(task)
            } catch (error) {
                if (attempt >= this.retry.attempts || !isRetryable(error)) throw error
                const ceiling = Math.min(
                    this.retry.maxDelayMs,
                    this.retry.baseDelayMs * 2 ** (attempt - 1)
                )
                const delay = Math.floor(ceiling * this.random())
                this.emit({
                    type: 'backup.retry',
                    operation,
                    key,
                    attempt,
                    delayMs: delay,
                    detail: error instanceof Error ? error.message : String(error)
                })
                await this.wait(delay)
            }
        }
    }

    /** @param {() => Promise<any>} task */
    async runRequest(task) {
        if (this.activeRequests >= this.concurrency) {
            await new Promise((resolve) => this.requestWaiters.push(() => resolve(undefined)))
        }
        if (this.abortController.signal.aborted) {
            this.requestWaiters.shift()?.()
            throw new Error('S3 backup is closing')
        }
        this.activeRequests++
        try {
            return await task()
        } finally {
            this.activeRequests--
            this.requestWaiters.shift()?.()
        }
    }

    /** Interruptible retry wait. @param {number} ms */
    async wait(ms) {
        const signal = this.abortController.signal
        if (signal.aborted) throw new Error('S3 backup is closing')
        if (ms === 0) return
        let rejectAbort
        const aborted = new Promise((_, reject) => {
            rejectAbort = () => reject(new Error('S3 backup is closing'))
            signal.addEventListener('abort', rejectAbort, { once: true })
        })
        try {
            await Promise.race([this.sleep(ms), aborted])
        } finally {
            if (rejectAbort) signal.removeEventListener('abort', rejectAbort)
        }
    }

    /** @param {() => Promise<void>} task @returns {Promise<void>} */
    enqueueMutation(task) {
        if (this.state !== 'open') {
            return Promise.reject(new Error(`S3 backup is ${this.state}`))
        }
        const run = this.mutationLane.then(task)
        this.mutationLane = run.catch(() => {})
        return run
    }

    /** @param {import('../observability/events.js').FyloEvent} event */
    emit(event) {
        emitFyloEvent(this.onEvent, event)
    }

    /**
     * Upload the given files (skipping any that no longer exist locally).
     * @param {string[]} absPaths
     * @returns {Promise<void>}
     */
    async mirror(absPaths) {
        await this.enqueueMutation(async () => {
            await mapLimit([...new Set(absPaths)], this.concurrency, async (absPath) => {
                const snapshot = await this.snapshot(absPath)
                if (!snapshot) return
                const key = this.key(absPath)
                const manifest = fileManifest(key, snapshot.bytes, snapshot.xattrs, snapshot.native)
                await Promise.all([
                    this.withRetry('write', key, () => this.client.write(key, snapshot.bytes)),
                    this.withRetry('write', this.manifestKey(key), () =>
                        this.client.write(this.manifestKey(key), manifest)
                    )
                ])
            })
        })
    }

    /**
     * Delete the S3 objects for the given local paths.
     * @param {string[]} absPaths
     * @returns {Promise<void>}
     */
    async remove(absPaths) {
        await this.enqueueMutation(async () => {
            await mapLimit([...new Set(absPaths)], this.concurrency, async (absPath) => {
                const key = this.key(absPath)
                await Promise.all([
                    this.withRetry('delete', key, () => this.client.delete(key)),
                    this.withRetry('delete', this.manifestKey(key), () =>
                        this.client.delete(this.manifestKey(key))
                    )
                ])
            })
        })
    }

    /**
     * Make S3 match the local root exactly: upload files that are missing in
     * S3 or whose size differs, and delete S3 objects with no local file.
     * Runs are serialized so an interval can't stack overlapping passes.
     * @returns {Promise<void>}
     */
    /** @param {'manual' | 'scheduled'} [source] */
    reconcile(source = 'manual') {
        this.nextSource = source
        return this.scheduler.trigger()
    }

    async reconcilePass() {
        const source = this.nextSource
        this.nextSource = 'manual'
        const startedAt = new Date().toISOString()
        this.status = { ...this.status, state: 'running', lastStartedAt: startedAt }
        this.emit({ type: 'backup.reconcile.started', source, startedAt })
        try {
            await this.enqueueMutation(() => this.reconcileOnce())
            const completedAt = new Date().toISOString()
            this.status = {
                ...this.status,
                state: 'idle',
                runs: this.status.runs + 1,
                lastSuccessAt: completedAt,
                lastError: undefined
            }
            this.emit({ type: 'backup.reconcile.succeeded', source, startedAt, completedAt })
        } catch (error) {
            const failedAt = new Date().toISOString()
            const detail = error instanceof Error ? error.message : String(error)
            this.status = {
                ...this.status,
                state: 'failed',
                runs: this.status.runs + 1,
                lastFailureAt: failedAt,
                lastError: detail
            }
            this.emit({ type: 'backup.reconcile.failed', source, startedAt, failedAt, detail })
            throw error
        }
    }

    /** @returns {Promise<void>} */
    async reconcileOnce() {
        return await this.reconcileAttempt(0)
    }

    /** @param {number} attempt @returns {Promise<void>} */
    async reconcileAttempt(attempt) {
        if (attempt >= 64) throw new Error('Unable to obtain a stable backup generation snapshot')
        let generationsBefore
        try {
            generationsBefore = await this.collectionGenerations()
        } catch (error) {
            if (
                !(error instanceof Error) ||
                !error.message.startsWith('Backup snapshot deferred')
            ) {
                throw error
            }
            await this.sleep(Math.min(2 ** attempt, 100))
            return await this.reconcileAttempt(attempt + 1)
        }
        /** @type {Map<string, { path: string, size: number }>} */
        const local = new Map()
        const walk = async (/** @type {string} */ directory) => {
            /** @type {import('node:fs').Dirent[]} */
            let entries
            try {
                entries = await readdir(directory, { withFileTypes: true })
            } catch (err) {
                if (/** @type {NodeJS.ErrnoException} */ (err)?.code === 'ENOENT') return
                throw err
            }
            for (const entry of entries) {
                if (entry.isSymbolicLink()) continue
                if (directory === this.root && entry.name === LOCAL_TRANSACTION_DIR) continue
                const target = path.join(directory, entry.name)
                if (isDurableWriteScratchPath(target)) continue
                if (entry.isDirectory()) await walk(target)
                else if (entry.isFile()) {
                    try {
                        local.set(this.key(target), {
                            path: target,
                            size: (await stat(target)).size
                        })
                    } catch (err) {
                        if (/** @type {NodeJS.ErrnoException} */ (err)?.code !== 'ENOENT') throw err
                    }
                }
            }
        }
        await walk(this.root)
        /** @type {Map<string, { bytes: Uint8Array, size: number, xattrs: Record<string, string>, native: { mode: number, mtimeMs: number } }>} */
        const materialized = new Map()
        let materializedBytes = 0
        for (const [key, file] of local) {
            const snapshot = await this.snapshot(file.path)
            if (!snapshot) {
                local.delete(key)
                continue
            }
            materializedBytes += snapshot.bytes.byteLength
            for (const value of Object.values(snapshot.xattrs)) {
                materializedBytes += Buffer.byteLength(value)
            }
            if (materializedBytes > this.maxReconcileSnapshotBytes) {
                throw new RangeError(
                    `S3 reconcile snapshot exceeds sync.s3.maxReconcileSnapshotBytes (${materializedBytes} > ${this.maxReconcileSnapshotBytes})`
                )
            }
            materialized.set(key, snapshot)
        }
        let generationsAfter
        try {
            generationsAfter = await this.collectionGenerations()
        } catch (error) {
            if (
                !(error instanceof Error) ||
                !error.message.startsWith('Backup snapshot deferred')
            ) {
                throw error
            }
            await this.sleep(Math.min(2 ** attempt, 100))
            return await this.reconcileAttempt(attempt + 1)
        }
        if (!this.sameGenerations(generationsBefore, generationsAfter)) {
            return await this.reconcileAttempt(attempt + 1)
        }

        // List the current S3 objects (key -> size) under the prefix.
        /** @type {Map<string, number>} */
        const remote = new Map()
        /** @type {Map<string, number>} */
        const remoteManifests = new Map()
        const listPrefix = this.prefix ? `${this.prefix}/` : ''
        /** @type {string | undefined} */
        let startAfter
        do {
            const page = await this.withRetry('list', listPrefix, () =>
                this.client.list({ prefix: listPrefix, startAfter })
            )
            const contents = page.contents ?? []
            for (const item of contents) {
                if (!item.key) continue
                if (this.isManifestKey(item.key)) remoteManifests.set(item.key, item.size ?? -1)
                else remote.set(item.key, item.size ?? -1)
            }
            if (page.isTruncated && !contents.at(-1)?.key) {
                throw new Error('S3 returned a truncated listing without a continuation key')
            }
            startAfter = page.isTruncated ? contents.at(-1)?.key : undefined
        } while (startAfter)

        // Listing can take long enough for non-transactional root files to
        // change. Validate the full materialized view once more before the
        // first remote mutation; any drift restarts the pass atomically.
        for (const [key, file] of local) {
            const current = await this.snapshot(file.path)
            const captured = materialized.get(key)
            if (
                !current ||
                !captured ||
                fileManifest(key, current.bytes, current.xattrs, current.native) !==
                    fileManifest(key, captured.bytes, captured.xattrs, captured.native)
            ) {
                return await this.reconcileAttempt(attempt + 1)
            }
        }

        // Upload new/changed files; delete remote objects with no local file.
        /** @type {Array<() => Promise<void>>} */
        const mutations = []
        for (const [key] of local) {
            const snapshot = materialized.get(key)
            if (!snapshot) continue
            const manifest = fileManifest(key, snapshot.bytes, snapshot.xattrs, snapshot.native)
            const manifestKey = this.manifestKey(key)
            let remoteManifest = ''
            if (remoteManifests.has(manifestKey)) {
                remoteManifest = new TextDecoder().decode(
                    await this.readObject(manifestKey, remoteManifests.get(manifestKey))
                )
            }
            if (remote.get(key) !== snapshot.size || remoteManifest !== manifest) {
                mutations.push(async () => {
                    await Promise.all([
                        this.withRetry('write', key, () => this.client.write(key, snapshot.bytes)),
                        this.withRetry('write', manifestKey, () =>
                            this.client.write(manifestKey, manifest)
                        )
                    ])
                })
            }
        }
        for (const key of remote.keys()) {
            if (!local.has(key)) {
                mutations.push(async () => {
                    await Promise.all([
                        this.withRetry('delete', key, () => this.client.delete(key)),
                        this.withRetry('delete', this.manifestKey(key), () =>
                            this.client.delete(this.manifestKey(key))
                        )
                    ])
                })
            }
        }
        for (const key of remoteManifests.keys()) {
            const encoded = key.slice(key.lastIndexOf('/') + 1, -'.json'.length)
            let dataKey
            try {
                dataKey = Buffer.from(encoded, 'base64url').toString()
            } catch {
                mutations.push(async () => {
                    await this.withRetry('delete', key, () => this.client.delete(key))
                })
                continue
            }
            if (!local.has(dataKey)) {
                mutations.push(async () => {
                    await this.withRetry('delete', key, () => this.client.delete(key))
                })
            }
        }
        await mapLimit(mutations, this.concurrency, (mutation) => mutation())
    }

    /** @returns {Promise<Map<string, number>>} */
    async collectionGenerations() {
        const result = new Map()
        for (const namespace of ['.collections', '.buckets']) {
            let entries = []
            try {
                entries = await readdir(path.join(this.root, namespace), { withFileTypes: true })
            } catch (error) {
                if (/** @type {NodeJS.ErrnoException} */ (error).code === 'ENOENT') continue
                throw error
            }
            for (const entry of entries) {
                if (!entry.isDirectory()) continue
                const state = await this.collectionGeneration(namespace, entry.name)
                if (state.state !== 'stable') {
                    throw new Error(`Backup snapshot deferred by active transaction: ${entry.name}`)
                }
                result.set(`${namespace}/${entry.name}`, state.generation)
            }
        }
        return result
    }

    /** @param {string} namespace @param {string} collection */
    async collectionGeneration(namespace, collection) {
        const relative = path.join(LOCAL_TRANSACTION_DIR, namespace, collection, 'state.json')
        const rootFd = this.rootFd ?? openDirectoryNoFollow(this.root)
        this.rootFd = rootFd
        let descriptor = null
        try {
            descriptor = openFileAtRootStrict(rootFd, relative)
            if (descriptor === null) return { generation: 0, state: 'stable' }
            const metadata = statSecureDescriptor(descriptor)
            if (!metadata.isFile())
                throw new Error(`Backup generation state is not a file: ${collection}`)
            if (metadata.size > MAX_GENERATION_STATE_BYTES) {
                throw new Error(`Backup generation state exceeds bounds: ${collection}`)
            }
            const bytes = Buffer.alloc(MAX_GENERATION_STATE_BYTES + 1)
            let offset = 0
            while (offset < bytes.length) {
                const count = readSecureDescriptor(
                    descriptor,
                    bytes,
                    offset,
                    bytes.length - offset,
                    offset
                )
                if (count === 0) break
                offset += count
            }
            if (offset > MAX_GENERATION_STATE_BYTES) {
                throw new Error(`Backup generation state exceeds bounds: ${collection}`)
            }
            const state = JSON.parse(bytes.subarray(0, offset).toString('utf8'))
            const keys = Object.keys(state).sort()
            const expected =
                state?.state === 'writing'
                    ? ['format', 'generation', 'state', 'transactionId']
                    : ['format', 'generation', 'state']
            if (
                state?.format !== 'fylo.collection-generation.v1' ||
                !Number.isSafeInteger(state.generation) ||
                state.generation < 0 ||
                !['stable', 'writing'].includes(state.state) ||
                keys.length !== expected.length ||
                keys.some((key, index) => key !== [...expected].sort()[index]) ||
                (state.state === 'writing' &&
                    (typeof state.transactionId !== 'string' ||
                        !state.transactionId ||
                        path.basename(state.transactionId) !== state.transactionId))
            ) {
                throw new Error(`Backup generation state is corrupt: ${collection}`)
            }
            return state
        } catch (error) {
            throw error
        } finally {
            if (descriptor !== null) closeSecureDescriptor(descriptor)
        }
    }

    /** @param {Map<string, number>} left @param {Map<string, number>} right */
    sameGenerations(left, right) {
        if (left.size !== right.size) return false
        for (const [key, generation] of left) {
            if (right.get(key) !== generation) return false
        }
        return true
    }

    /** Start the periodic reconcile timer (no-op unless an interval is set). */
    start() {
        this.scheduler.start()
    }

    /** Stop new work, cancel retries, drain active mutations, then release the root descriptor. */
    async close() {
        if (this.state === 'closed') return
        this.state = 'closing'
        this.abortController.abort()
        /** @type {unknown} */
        let failure
        try {
            await this.scheduler.close()
        } catch (error) {
            failure = error
        }
        await this.mutationLane
        if (this.rootFd !== undefined) closeSecureDescriptor(this.rootFd)
        this.rootFd = undefined
        this.state = 'closed'
        this.status = { ...this.status, state: 'closed' }
        if (failure !== undefined) throw failure
    }

    /** @deprecated use close(), which drains asynchronous work safely. */
    async stop() {
        await this.close()
    }
}
