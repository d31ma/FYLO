import { createMemoryFilesystem } from './core/memory-filesystem.js'
import { createOpfsFilesystem } from './opfs-filesystem.js'
import { FsaFilesystem, createOverlayFilesystem } from './fsa-filesystem.js'

/**
 * @typedef {'memory' | 'opfs' | { type: 'memory' } | { type: 'opfs' } | { type: 'fsa', handle: FileSystemDirectoryHandle, access?: 'readwrite' | 'overlay' }} BrowserStorage
 * @typedef {{ type: 'memory' } | { type: 'opfs' } | { type: 'fsa', handle: FileSystemDirectoryHandle, access: 'readwrite' | 'overlay' }} NormalizedBrowserStorage
 */

/**
 * @param {BrowserStorage} storage
 * @returns {NormalizedBrowserStorage}
 */
export function normalizeBrowserStorage(storage) {
    const value = typeof storage === 'string' ? { type: storage } : storage
    if (!value || typeof value !== 'object')
        throw new Error('Invalid browser storage configuration')
    if (value.type === 'memory' || value.type === 'opfs') return { type: value.type }
    if (value.type !== 'fsa') throw new Error(`Unsupported browser storage type: ${value.type}`)
    if (!value.handle || value.handle.kind !== 'directory') {
        throw new Error('File System Access storage requires a directory handle')
    }
    const access = value.access ?? 'overlay'
    if (access !== 'overlay' && access !== 'readwrite') {
        throw new Error(`Unsupported File System Access mode: ${access}`)
    }
    return { type: 'fsa', handle: value.handle, access }
}

/**
 * @param {NormalizedBrowserStorage} storage
 * @param {string} namespace
 * @returns {import('./core/filesystem.js').FyloFilesystem}
 */
export function createBrowserFilesystem(storage, namespace) {
    if (storage.type === 'memory') return createMemoryFilesystem()
    if (storage.type === 'opfs') return createOpfsFilesystem({ namespace })
    const direct = new FsaFilesystem(storage.handle)
    return storage.access === 'readwrite' ? direct : createOverlayFilesystem(direct)
}
