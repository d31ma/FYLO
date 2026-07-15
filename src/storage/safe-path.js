import path from 'node:path'
import { lstat, mkdir, realpath } from 'node:fs/promises'
import { assertPathInside } from '../core/doc-id.js'

/**
 * Rejects links/reparse-point escapes in every component below a trusted FYLO
 * root. Missing parent directories may be created one at a time, with each
 * component re-validated before continuing.
 *
 * @param {string} storageRoot trusted FYLO working-tree root
 * @param {string} target
 * @param {{ allowMissingFinal?: boolean, createParentDirectories?: boolean, finalType?: 'file' | 'directory' }} [options]
 * @returns {Promise<void>}
 */
export async function assertSafeStoragePath(storageRoot, target, options = {}) {
    const absoluteRoot = path.resolve(storageRoot)
    const absoluteTarget = path.resolve(target)
    assertPathInside(absoluteRoot, absoluteTarget)
    const relative = path.relative(absoluteRoot, absoluteTarget)
    const segments = relative ? relative.split(path.sep) : []
    const canonicalRoot = await realpath(absoluteRoot)
    let current = absoluteRoot

    for (let index = 0; index < segments.length; index++) {
        current = path.join(current, segments[index])
        const final = index === segments.length - 1
        let metadata
        try {
            metadata = await lstat(current)
        } catch (error) {
            if (/** @type {NodeJS.ErrnoException} */ (error).code !== 'ENOENT') throw error
            if (!final && options.createParentDirectories) {
                try {
                    await mkdir(current)
                } catch (mkdirError) {
                    if (/** @type {NodeJS.ErrnoException} */ (mkdirError).code !== 'EEXIST') {
                        throw mkdirError
                    }
                }
                metadata = await lstat(current)
            } else if (final && options.allowMissingFinal) {
                assertPathInside(canonicalRoot, await realpath(path.dirname(current)))
                return
            } else {
                throw error
            }
        }

        if (metadata.isSymbolicLink()) {
            throw new Error(`Storage path contains a symbolic link or reparse point: ${current}`)
        }
        if (!final && !metadata.isDirectory()) {
            throw new Error(`Storage path component is not a directory: ${current}`)
        }
        if (final && options.finalType === 'file' && !metadata.isFile()) {
            throw new Error(`Storage target is not a regular file: ${current}`)
        }
        if (final && options.finalType === 'directory' && !metadata.isDirectory()) {
            throw new Error(`Storage target is not a directory: ${current}`)
        }
        assertPathInside(canonicalRoot, await realpath(current))
    }
}
