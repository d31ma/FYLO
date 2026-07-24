import { open, mkdir, realpath, unlink } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { writeDurable } from '../storage/durable.js'
import { tryAcquireProcessFileLock } from '../storage/secure-open.js'

const MAX_LEASE_METADATA_BYTES = 4096

export class FyloRootLeaseError extends Error {
    /** @type {'EROOTLOCKED' | 'EROOTLEASELOST'} */
    code

    /**
     * @param {'EROOTLOCKED' | 'EROOTLEASELOST'} code
     * @param {string} message
     */
    constructor(code, message) {
        super(message)
        this.name = 'FyloRootLeaseError'
        this.code = code
    }
}

/** @param {string} canonicalRoot */
export function rootLeasePaths(canonicalRoot) {
    const parent = path.dirname(canonicalRoot)
    const basename = path.basename(canonicalRoot) || 'root'
    const sentinel = path.join(parent, `.${basename}.fylo-root-owner.lock`)
    return { sentinel, metadata: `${sentinel}.json` }
}

/** @param {string} metadataPath */
async function readLeaseMetadata(metadataPath) {
    let handle
    try {
        handle = await open(metadataPath, 'r')
        const info = await handle.stat()
        if (!info.isFile() || info.size > MAX_LEASE_METADATA_BYTES) return null
        const bytes = Buffer.alloc(Number(info.size))
        const { bytesRead } = await handle.read(bytes, 0, bytes.byteLength, 0)
        return JSON.parse(bytes.subarray(0, bytesRead).toString('utf8'))
    } catch {
        return null
    } finally {
        await handle?.close()
    }
}

export class FyloRootLease {
    /**
     * @param {string} root
     * @param {string} owner
     * @param {{ sentinel: string, metadata: string }} paths
     * @param {() => void} releaseKernelLock
     */
    constructor(root, owner, paths, releaseKernelLock) {
        this.root = root
        this.owner = owner
        this.paths = paths
        this.releaseKernelLock = releaseKernelLock
        this.released = false
    }

    async assertOwned() {
        if (this.released) {
            throw new FyloRootLeaseError(
                'EROOTLEASELOST',
                `Exclusive ownership of FYLO root ${this.root} has been released`
            )
        }
        const metadata = await readLeaseMetadata(this.paths.metadata)
        if (
            !metadata ||
            metadata.version !== 1 ||
            metadata.root !== this.root ||
            metadata.owner !== this.owner
        ) {
            throw new FyloRootLeaseError(
                'EROOTLEASELOST',
                `Exclusive ownership of FYLO root ${this.root} was lost`
            )
        }
    }

    async release() {
        if (this.released) return
        this.released = true
        try {
            const metadata = await readLeaseMetadata(this.paths.metadata)
            if (metadata?.owner === this.owner) {
                try {
                    await unlink(this.paths.metadata)
                } catch (error) {
                    if (/** @type {NodeJS.ErrnoException} */ (error).code !== 'ENOENT') throw error
                }
            }
        } finally {
            this.releaseKernelLock()
        }
    }
}

/**
 * Acquires a kernel-owned lease on the canonical root. The persistent
 * sentinel may remain after shutdown, but the OS releases its lock on close,
 * crash, or SIGKILL. A unique metadata generation fences a former owner if
 * the sentinel is externally replaced and a successor acquires it.
 *
 * @param {string} root
 */
export async function acquireRootLease(root) {
    const requested = path.resolve(root)
    await mkdir(requested, { recursive: true })
    const canonicalRoot = await realpath(requested)
    return await acquireCanonicalRootLease(canonicalRoot)
}

/**
 * Reserves a root path that must not exist yet. Used by offline restore so a
 * live process cannot open the destination while verified staging is prepared.
 *
 * @param {string} root
 */
export async function acquireRootReservation(root) {
    const requested = path.resolve(root)
    const canonicalParent = await realpath(path.dirname(requested))
    const canonicalRoot = path.join(canonicalParent, path.basename(requested))
    return await acquireCanonicalRootLease(canonicalRoot)
}

/** @param {string} canonicalRoot */
async function acquireCanonicalRootLease(canonicalRoot) {
    const paths = rootLeasePaths(canonicalRoot)
    const releaseKernelLock = tryAcquireProcessFileLock(paths.sentinel)
    if (!releaseKernelLock) {
        throw new FyloRootLeaseError(
            'EROOTLOCKED',
            `FYLO root already has a live exclusive owner: ${canonicalRoot}`
        )
    }

    const owner = Bun.randomUUIDv7()
    try {
        await writeDurable(
            paths.metadata,
            JSON.stringify({
                version: 1,
                root: canonicalRoot,
                owner,
                pid: process.pid,
                host: os.hostname(),
                acquiredAt: Date.now()
            })
        )
        return new FyloRootLease(canonicalRoot, owner, paths, releaseKernelLock)
    } catch (error) {
        releaseKernelLock()
        throw error
    }
}
