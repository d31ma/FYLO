import { mkdir, open, rename, rm } from 'node:fs/promises'
import path from 'node:path'

const DURABLE_SCRATCH_SUFFIX = /[.][0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}[.]tmp$/i

/**
 * Identifies the unique sibling created by {@link DurableFileWriter} before
 * fsync + atomic rename. Scanners must not treat this transient file as data.
 *
 * @param {string} value basename or path
 * @returns {boolean}
 */
export function isDurableWriteScratchPath(value) {
    return DURABLE_SCRATCH_SUFFIX.test(path.basename(value))
}

/**
 * Detects platforms/filesystems that do not support fsync on directory handles.
 *
 * @param {unknown} error
 * @returns {boolean}
 */
function isUnsupportedDirectorySync(error) {
    return (
        process.platform === 'win32' &&
        error instanceof Error &&
        'code' in error &&
        ['EPERM', 'EINVAL', 'ENOTSUP'].includes(String(error.code))
    )
}

/**
 * Atomic file writer that fsyncs file contents and, when supported, the parent
 * directory.
 */
export class DurableFileWriter {
    /**
     * Create the scratch file, tolerating a transient missing parent directory.
     * Under heavy concurrent writes into the same bucket, the just-created dir
     * can briefly not be visible at open time; re-create and retry. Runs before
     * any bytes are written, so it does not affect durability guarantees.
     *
     * @param {string} scratchPath
     * @param {string} targetDirectory
     * @returns {Promise<import('node:fs/promises').FileHandle>}
     */
    async openScratch(scratchPath, targetDirectory) {
        for (let attempt = 0; ; attempt++) {
            try {
                return await open(scratchPath, 'wx')
            } catch (error) {
                const code = error instanceof Error ? /** @type {any} */ (error).code : undefined
                if (attempt >= 8 || code !== 'ENOENT') throw error
                // Yield so a concurrent recursive-mkdir of the same bucket can
                // settle before we re-create and retry.
                await new Promise((resolve) => setTimeout(resolve, attempt))
                await mkdir(targetDirectory, { recursive: true })
            }
        }
    }

    /**
     * @param {string} target
     * @param {string | Uint8Array} data
     * @returns {Promise<void>}
     */
    async write(target, data) {
        const targetDirectory = path.dirname(target)
        await mkdir(targetDirectory, { recursive: true })
        const scratchPath = `${target}.${Bun.randomUUIDv7()}.tmp`
        const fileHandle = await this.openScratch(scratchPath, targetDirectory)
        try {
            try {
                await fileHandle.writeFile(data)
                await fileHandle.sync()
            } finally {
                await fileHandle.close()
            }
            await rename(scratchPath, target)
        } catch (error) {
            await rm(scratchPath, { force: true })
            throw error
        }
        const directoryHandle = await open(targetDirectory, 'r')
        try {
            await directoryHandle.sync()
        } catch (error) {
            if (!isUnsupportedDirectorySync(error)) throw error
        } finally {
            await directoryHandle.close()
        }
    }

    /**
     * Streams bytes into a crash-safe file while computing its content digest.
     * Memory usage remains bounded by the producer's chunk size.
     *
     * @param {string} target
     * @param {ReadableStream<Uint8Array>} stream
     * @param {{ maxBytes?: number }} [options]
     * @returns {Promise<{ contentLength: number, checksumSHA256: string }>}
     */
    async writeStream(target, stream, options = {}) {
        const targetDirectory = path.dirname(target)
        await mkdir(targetDirectory, { recursive: true })
        const scratchPath = `${target}.${Bun.randomUUIDv7()}.tmp`
        const fileHandle = await this.openScratch(scratchPath, targetDirectory)
        const hasher = new Bun.CryptoHasher('sha256')
        let contentLength = 0
        try {
            for await (const value of /** @type {AsyncIterable<Uint8Array>} */ (
                /** @type {unknown} */ (stream)
            )) {
                const chunk = value instanceof Uint8Array ? value : new Uint8Array(value)
                contentLength += chunk.byteLength
                if (options.maxBytes !== undefined && contentLength > options.maxBytes) {
                    throw new Error(`Raw file exceeded ${options.maxBytes} bytes`)
                }
                hasher.update(chunk)
                await fileHandle.write(chunk)
            }
            await fileHandle.sync()
        } catch (error) {
            await fileHandle.close()
            await rm(scratchPath, { force: true })
            throw error
        }
        await fileHandle.close()
        try {
            await rename(scratchPath, target)
        } catch (error) {
            await rm(scratchPath, { force: true }) // don't orphan the scratch file
            throw error
        }
        const directoryHandle = await open(targetDirectory, 'r')
        try {
            await directoryHandle.sync()
        } catch (error) {
            if (!isUnsupportedDirectorySync(error)) throw error
        } finally {
            await directoryHandle.close()
        }
        return {
            contentLength,
            checksumSHA256: hasher.digest('hex')
        }
    }
}

/** Shared durable writer instance. */
export const durableFileWriter = new DurableFileWriter()

/**
 * Writes `data` to `target` with crash-safe durability guarantees.
 *
 * Pattern: write to a unique sibling scratch file, fsync the file, rename
 * into place, then fsync the parent directory so the rename itself is durable.
 *
 * After this resolves, the content at `target` survives a crash or
 * power loss on ext4/xfs/APFS (assuming the underlying disk honors fsync).
 *
 * Creates parent directories as needed.
 *
 * @param {string} target
 * @param {string | Uint8Array} data
 * @returns {Promise<void>}
 */
export async function writeDurable(target, data) {
    await durableFileWriter.write(target, data)
}

/**
 * @param {string} target
 * @param {ReadableStream<Uint8Array>} stream
 * @param {{ maxBytes?: number }} [options]
 * @returns {Promise<{ contentLength: number, checksumSHA256: string }>}
 */
export async function writeDurableStream(target, stream, options) {
    return await durableFileWriter.writeStream(target, stream, options)
}
