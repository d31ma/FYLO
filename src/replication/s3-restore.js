/**
 * Production-safe recovery for whole-root S3 backups created by
 * {@link FyloS3Backup}. Recovery is intentionally explicit: it restores only
 * into a new path, validates every sidecar manifest while streaming bytes, and
 * promotes the complete staging directory with one rename.
 */

import path from 'node:path'
import { createHash } from 'node:crypto'
import { lstat, mkdir, rename, rm } from 'node:fs/promises'
import { resolveS3BackupOptions } from './s3-backup.js'
import { writeDurableStream } from '../storage/durable.js'
import { setXattr } from '../storage/xattr.js'
import { readAccessDescriptor, restoreAccessDescriptor } from '../security/access.js'

const BACKUP_METADATA_DIR = '.fylo-backup/xattrs/'
const DEFAULT_MAX_OBJECT_BYTES = 1024 ** 4
const DEFAULT_MAX_MANIFEST_BYTES = 1024 ** 2
const DEFAULT_MAX_XATTR_BYTES = 1024 ** 2

/** @typedef {'list' | 'download' | 'verify' | 'retry' | 'promote' | 'complete' | 'failed'} FyloS3RestorePhase */

/**
 * @typedef {object} FyloS3RestoreStatus
 * @property {FyloS3RestorePhase} phase
 * @property {number} files
 * @property {number} bytes
 * @property {string=} key
 * @property {number=} attempt
 * @property {string=} message
 */

/**
 * @typedef {object} FyloS3RestoreOptions
 * @property {number=} concurrency maximum simultaneous object recoveries
 * @property {number=} maxObjectBytes maximum accepted size for one data object
 * @property {number=} maxManifestBytes maximum accepted sidecar manifest size
 * @property {number=} maxXattrBytes maximum decoded xattr bytes per object
 * @property {{ attempts?: number, baseDelayMs?: number }=} retry
 * @property {AbortSignal=} signal
 * @property {(status: FyloS3RestoreStatus) => void=} onStatus
 */

/** @typedef {{ concurrency: number, maxObjectBytes: number, maxManifestBytes: number, maxXattrBytes: number, attempts: number, baseDelayMs: number, signal?: AbortSignal }} ResolvedRestoreOptions */
/** @typedef {{ size: number, sha256: string, xattrs: Array<[string, Uint8Array]> }} ValidatedManifest */
/** @typedef {(status: Partial<FyloS3RestoreStatus> & Pick<FyloS3RestoreStatus, 'phase'>) => void} StatusReporter */

/** @param {unknown} value @param {string} name */
function assertPositiveInteger(value, name) {
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 1) {
        throw new TypeError(`${name} must be a positive safe integer`)
    }
}

/** @param {unknown} value @param {string} name */
function assertNonNegativeInteger(value, name) {
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
        throw new TypeError(`${name} must be a non-negative safe integer`)
    }
}

/** @param {AbortSignal | undefined} signal */
function assertNotAborted(signal) {
    if (signal?.aborted) throw signal.reason ?? new DOMException('Restore aborted', 'AbortError')
}

/** @param {string} target @returns {Promise<boolean>} */
async function pathExists(target) {
    try {
        await lstat(target)
        return true
    } catch (error) {
        if (/** @type {NodeJS.ErrnoException} */ (error).code === 'ENOENT') return false
        throw error
    }
}

/**
 * @param {ReadableStream<Uint8Array>} stream
 * @param {number} maximum
 * @param {string} label
 * @param {AbortSignal | undefined} signal
 * @returns {Promise<Buffer>}
 */
async function readBounded(stream, maximum, label, signal) {
    const chunks = []
    let length = 0
    for await (const value of /** @type {AsyncIterable<Uint8Array>} */ (
        /** @type {unknown} */ (stream)
    )) {
        assertNotAborted(signal)
        const chunk = value instanceof Uint8Array ? value : new Uint8Array(value)
        length += chunk.byteLength
        if (length > maximum) throw new Error(`${label} exceeded ${maximum} bytes`)
        chunks.push(chunk)
    }
    return Buffer.concat(chunks, length)
}

/** @param {unknown} value @param {string} name @returns {Buffer} */
function decodeBase64(value, name) {
    if (
        typeof value !== 'string' ||
        !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)
    ) {
        throw new Error(`invalid base64 value for xattr ${name}`)
    }
    return Buffer.from(value, 'base64')
}

/**
 * @param {unknown} value
 * @param {string} dataKey
 * @param {ResolvedRestoreOptions} limits
 * @returns {ValidatedManifest}
 */
function validateManifest(value, dataKey, limits) {
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
        throw new Error(`invalid backup manifest for ${dataKey}`)
    }
    const manifest = /** @type {Record<string, unknown>} */ (value)
    if (manifest.version !== 1 || manifest.dataKey !== dataKey) {
        throw new Error(`invalid backup manifest identity for ${dataKey}`)
    }
    if (!Number.isSafeInteger(manifest.size) || Number(manifest.size) < 0) {
        throw new Error(`invalid backup manifest size for ${dataKey}`)
    }
    if (Number(manifest.size) > limits.maxObjectBytes) {
        throw new Error(`backup object ${dataKey} exceeds ${limits.maxObjectBytes} bytes`)
    }
    if (typeof manifest.sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(manifest.sha256)) {
        throw new Error(`invalid backup manifest checksum for ${dataKey}`)
    }
    if (!manifest.xattrs || typeof manifest.xattrs !== 'object' || Array.isArray(manifest.xattrs)) {
        throw new Error(`invalid backup xattrs for ${dataKey}`)
    }
    const entries = Object.entries(/** @type {Record<string, unknown>} */ (manifest.xattrs))
    if (entries.length > 256) throw new Error(`too many backup xattrs for ${dataKey}`)
    let xattrBytes = 0
    /** @type {Array<[string, Uint8Array]>} */
    const xattrs = entries.map(([name, encoded]) => {
        if (!name || name.length > 255 || /[\0\r\n]/.test(name)) {
            throw new Error(`invalid backup xattr name for ${dataKey}`)
        }
        const bytes = decodeBase64(encoded, name)
        xattrBytes += bytes.byteLength
        if (xattrBytes > limits.maxXattrBytes) {
            throw new Error(`backup xattrs for ${dataKey} exceed ${limits.maxXattrBytes} bytes`)
        }
        return [name, bytes]
    })
    return {
        size: Number(manifest.size),
        sha256: manifest.sha256,
        xattrs
    }
}

/**
 * @template T
 * @param {T[]} values
 * @param {number} concurrency
 * @param {(value: T) => Promise<void>} operation
 */
async function mapLimit(values, concurrency, operation) {
    let cursor = 0
    let failed = false
    /** @type {unknown} */
    let firstError
    const workers = Array.from({ length: Math.min(concurrency, values.length) }, async () => {
        while (cursor < values.length && !failed) {
            const index = cursor++
            try {
                await operation(values[index])
            } catch (error) {
                if (!failed) {
                    failed = true
                    firstError = error
                }
            }
        }
    })
    await Promise.all(workers)
    if (failed) throw firstError
}

export class FyloS3Restore {
    /**
     * @param {import('./s3-backup.js').FyloS3BackupOptions} options
     * @param {string} destination absolute path that must not already exist
     * @param {{ client?: Bun.S3Client }} [deps]
     */
    constructor(options, destination, deps = {}) {
        if (process.platform === 'win32') {
            throw new Error(
                'S3 recovery is unavailable on Windows because backup xattrs cannot be restored safely'
            )
        }
        this.options = resolveS3BackupOptions(options)
        if (!this.options.bucket || typeof this.options.bucket !== 'string') {
            throw new TypeError('S3 restore bucket must be a non-empty string')
        }
        if (typeof this.options.prefix !== 'string' || !this.options.prefix) {
            throw new TypeError('S3 restore prefix must be a non-empty string')
        }
        this.prefix = this.options.prefix.replace(/^\/+|\/+$/g, '')
        if (
            !this.prefix ||
            this.prefix.includes('\\') ||
            this.prefix.split('/').some((part) => !part || part === '.' || part === '..') ||
            /[\0-\x1f\x7f]/.test(this.prefix)
        ) {
            throw new TypeError('S3 restore prefix must be a normalized S3 key prefix')
        }
        if (!path.isAbsolute(destination)) {
            throw new TypeError('S3 restore destination must be an absolute path')
        }
        this.destination = path.resolve(destination)
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
    }

    /** @param {string} dataKey @returns {string} */
    manifestKey(dataKey) {
        return `${this.prefix}/${BACKUP_METADATA_DIR}${Buffer.from(dataKey).toString('base64url')}.json`
    }

    /** @param {string} dataKey @returns {string} */
    relativePath(dataKey) {
        const base = `${this.prefix}/`
        if (!dataKey.startsWith(base))
            throw new Error(`unsafe S3 object key outside restore prefix: ${dataKey}`)
        const relative = dataKey.slice(base.length)
        const parts = relative.split('/')
        if (
            !relative ||
            relative.startsWith('/') ||
            relative.includes('\\') ||
            parts.some((part) => !part || part === '.' || part === '..') ||
            /[\0-\x1f\x7f]/.test(relative)
        ) {
            throw new Error(`unsafe S3 object key: ${dataKey}`)
        }
        return parts.join(path.sep)
    }

    /** @param {string} key @returns {boolean} */
    isManifestKey(key) {
        return key.startsWith(`${this.prefix}/${BACKUP_METADATA_DIR}`)
    }

    /**
     * @template T
     * @param {() => Promise<T>} operation
     * @param {ResolvedRestoreOptions} settings
     * @param {StatusReporter} status
     * @returns {Promise<T>}
     */
    async withRetry(operation, settings, status) {
        let lastError
        for (let attempt = 1; attempt <= settings.attempts; attempt++) {
            assertNotAborted(settings.signal)
            try {
                return await operation()
            } catch (error) {
                lastError = error
                assertNotAborted(settings.signal)
                if (attempt === settings.attempts) break
                status({
                    phase: 'retry',
                    attempt,
                    message: error instanceof Error ? error.message : String(error)
                })
                const delay = settings.baseDelayMs * 2 ** (attempt - 1)
                if (delay) await Bun.sleep(delay)
            }
        }
        throw lastError
    }

    /**
     * @param {string} dataKey
     * @param {ResolvedRestoreOptions} settings
     * @param {StatusReporter} status
     * @returns {Promise<ValidatedManifest>}
     */
    async readManifest(dataKey, settings, status) {
        const bytes = await this.withRetry(
            () =>
                readBounded(
                    this.client.file(this.manifestKey(dataKey)).stream(),
                    settings.maxManifestBytes,
                    `backup manifest ${dataKey}`,
                    settings.signal
                ),
            settings,
            status
        )
        let parsed
        try {
            parsed = JSON.parse(bytes.toString('utf8'))
        } catch (cause) {
            throw new Error(`invalid backup manifest JSON for ${dataKey}`, { cause })
        }
        return validateManifest(parsed, dataKey, settings)
    }

    /** @param {FyloS3RestoreOptions} options @returns {ResolvedRestoreOptions} */
    settings(options) {
        const settings = {
            concurrency: options.concurrency ?? 4,
            maxObjectBytes: options.maxObjectBytes ?? DEFAULT_MAX_OBJECT_BYTES,
            maxManifestBytes: options.maxManifestBytes ?? DEFAULT_MAX_MANIFEST_BYTES,
            maxXattrBytes: options.maxXattrBytes ?? DEFAULT_MAX_XATTR_BYTES,
            attempts: options.retry?.attempts ?? 3,
            baseDelayMs: options.retry?.baseDelayMs ?? 100,
            signal: options.signal
        }
        for (const [name, value] of Object.entries(settings)) {
            if (name === 'signal') continue
            if (name === 'baseDelayMs') assertNonNegativeInteger(value, `S3 restore ${name}`)
            else assertPositiveInteger(value, `S3 restore ${name}`)
        }
        return settings
    }

    /**
     * @param {ResolvedRestoreOptions} settings
     * @param {StatusReporter} status
     * @returns {AsyncGenerator<string[]>}
     */
    async *dataPages(settings, status) {
        const listPrefix = `${this.prefix}/`
        /** @type {string | undefined} */
        let startAfter
        do {
            assertNotAborted(settings.signal)
            const page = await this.withRetry(
                () => this.client.list({ prefix: listPrefix, startAfter }),
                settings,
                status
            )
            const contents = page.contents ?? []
            status({ phase: 'list' })
            yield contents
                .filter((item) => item.key && !this.isManifestKey(item.key))
                .map((item) => item.key)
            if (!page.isTruncated) return
            const next = contents.at(-1)?.key
            if (!next || next === startAfter)
                throw new Error('S3 listing was truncated without a continuation key')
            startAfter = next
        } while (startAfter)
    }

    /**
     * @param {'restore' | 'verify'} mode
     * @param {FyloS3RestoreOptions} [options]
     * @returns {Promise<{ status: string, files: number, bytes: number }>}
     */
    async run(mode, options = {}) {
        const settings = this.settings(options)
        const counters = { files: 0, bytes: 0 }
        /** @type {StatusReporter} */
        const status = (event) => options.onStatus?.({ ...counters, ...event })
        const staging = `${this.destination}.fylo-restore-${Bun.randomUUIDv7()}.tmp`
        try {
            if (mode === 'restore') {
                if (await pathExists(this.destination)) {
                    throw new Error(`S3 restore destination already exists: ${this.destination}`)
                }
                await mkdir(staging, { recursive: false })
            }
            for await (const keys of this.dataPages(settings, status)) {
                await mapLimit(keys, settings.concurrency, async (dataKey) => {
                    assertNotAborted(settings.signal)
                    const relative = this.relativePath(dataKey)
                    const manifest = await this.readManifest(dataKey, settings, status)
                    status({ phase: 'download', key: dataKey })
                    if (mode === 'restore') {
                        const target = path.join(staging, relative)
                        const result = await this.withRetry(
                            () =>
                                writeDurableStream(target, this.client.file(dataKey).stream(), {
                                    maxBytes: manifest.size
                                }),
                            settings,
                            status
                        )
                        if (result.contentLength !== manifest.size)
                            throw new Error(`size mismatch for backup object ${dataKey}`)
                        if (result.checksumSHA256 !== manifest.sha256)
                            throw new Error(`checksum mismatch for backup object ${dataKey}`)
                        for (const [name, value] of manifest.xattrs) setXattr(target, name, value)
                        await restoreAccessDescriptor(target, await readAccessDescriptor(target))
                    } else {
                        let length = 0
                        await this.withRetry(
                            async () => {
                                length = 0
                                const retryHasher = createHash('sha256')
                                for await (const value of /** @type {AsyncIterable<Uint8Array>} */ (
                                    /** @type {unknown} */ (this.client.file(dataKey).stream())
                                )) {
                                    assertNotAborted(settings.signal)
                                    const chunk =
                                        value instanceof Uint8Array ? value : new Uint8Array(value)
                                    length += chunk.byteLength
                                    if (length > manifest.size)
                                        throw new Error(
                                            `size mismatch for backup object ${dataKey}`
                                        )
                                    retryHasher.update(chunk)
                                }
                                const digest = retryHasher.digest('hex')
                                if (length !== manifest.size)
                                    throw new Error(`size mismatch for backup object ${dataKey}`)
                                if (digest !== manifest.sha256)
                                    throw new Error(
                                        `checksum mismatch for backup object ${dataKey}`
                                    )
                            },
                            settings,
                            status
                        )
                    }
                    counters.files++
                    counters.bytes += manifest.size
                    status({ phase: 'verify', key: dataKey })
                })
            }
            if (counters.files === 0) {
                throw new Error(`S3 backup prefix contains no data objects: ${this.prefix}`)
            }
            if (mode === 'restore') {
                status({ phase: 'promote' })
                if (await pathExists(this.destination))
                    throw new Error(`S3 restore destination already exists: ${this.destination}`)
                await rename(staging, this.destination)
            }
            const result = { status: mode === 'restore' ? 'complete' : 'verified', ...counters }
            status({ phase: 'complete' })
            return result
        } catch (error) {
            status({
                phase: 'failed',
                message: error instanceof Error ? error.message : String(error)
            })
            if (mode === 'restore') await rm(staging, { recursive: true, force: true })
            throw error
        }
    }

    /** @param {FyloS3RestoreOptions} [options] */
    async restore(options = {}) {
        return await this.run('restore', options)
    }

    /** @param {FyloS3RestoreOptions} [options] */
    async verify(options = {}) {
        return await this.run('verify', options)
    }
}
