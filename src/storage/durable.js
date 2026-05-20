import { mkdir, open, rename } from 'node:fs/promises'
import path from 'node:path'

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
     * @param {string} target
     * @param {string | Uint8Array} data
     * @returns {Promise<void>}
     */
    async write(target, data) {
        const targetDirectory = path.dirname(target)
        await mkdir(targetDirectory, { recursive: true })
        const scratchPath = `${target}.tmp`
        const fileHandle = await open(scratchPath, 'w')
        try {
            await fileHandle.writeFile(data)
            await fileHandle.sync()
        } finally {
            await fileHandle.close()
        }
        await rename(scratchPath, target)
        const directoryHandle = await open(targetDirectory, 'r')
        try {
            await directoryHandle.sync()
        } catch (error) {
            if (!isUnsupportedDirectorySync(error)) throw error
        } finally {
            await directoryHandle.close()
        }
    }
}

/** Shared durable writer instance. */
export const durableFileWriter = new DurableFileWriter()

/**
 * Writes `data` to `target` with crash-safe durability guarantees.
 *
 * Pattern: write to `<target>.tmp`, fsync the file, rename into place,
 * then fsync the parent directory so the rename itself is durable.
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
