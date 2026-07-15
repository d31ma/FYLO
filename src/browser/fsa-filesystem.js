import { OpfsFilesystem } from './opfs-filesystem.js'
import { createMemoryFilesystem } from './core/memory-filesystem.js'
import { normalize } from './core/path.js'

/**
 * @typedef {import('./core/filesystem.js').FyloFilesystem} FyloFilesystem
 */

/**
 * `FyloFilesystem` over a real directory picked with the File System Access
 * API. A picked `FileSystemDirectoryHandle` exposes the same interface OPFS
 * roots do, so all traversal/read/write mechanics are inherited — only the
 * root differs. Chromium-only (Firefox/Safari implement just the OPFS half).
 */
export class FsaFilesystem extends OpfsFilesystem {
    /** @param {FileSystemDirectoryHandle} rootHandle */
    constructor(rootHandle) {
        super()
        this.rootHandle = rootHandle
    }

    /** @returns {Promise<FileSystemDirectoryHandle>} */
    async root() {
        return this.rootHandle
    }
}

/**
 * Copy-on-write overlay: reads fall through to `base`, every write lands in
 * memory. Lets the engine "write" index files and journals against a
 * read-only real root (indexes are accelerators — rebuilding them into RAM
 * is the browser-side expression of FYLO's rebuildability principle).
 *
 * ponytail: removals are tracked per exact path, not per subtree — rmdir of a
 * base directory hides the directory itself but not deep base descendants
 * looked up directly. The engine never does that against a read-only root.
 *
 * @param {FyloFilesystem} base
 * @returns {FyloFilesystem}
 */
export function createOverlayFilesystem(base) {
    const layer = createMemoryFilesystem()
    /** @type {Set<string>} removed base paths (exact, normalized) */
    const removed = new Set()
    const key = (/** @type {string} */ path) => normalize(path)
    const inBase = async (/** @type {string} */ path) =>
        !removed.has(key(path)) && (await base.exists(path))

    /** Seed the overlay with base content so an append/move sees prior bytes. */
    const copyUp = async (/** @type {string} */ path) => {
        if (!(await layer.exists(path)) && (await inBase(path))) {
            await layer.writeBytes(path, await base.readBytes(path))
        }
    }

    return {
        async exists(path) {
            return (await layer.exists(path)) || (await inBase(path))
        },
        async isDirectory(path) {
            if (await layer.exists(path)) return await layer.isDirectory(path)
            if (removed.has(key(path))) return false
            return await base.isDirectory(path)
        },
        async mtimeMs(path) {
            if (await layer.exists(path)) return await layer.mtimeMs(path)
            return await base.mtimeMs(path)
        },
        async size(path) {
            if (await layer.exists(path)) return await layer.size(path)
            return await base.size(path)
        },
        async mkdir(path, options) {
            await layer.mkdir(path, options)
        },
        async list(path) {
            /** @type {Set<string>} */
            const names = new Set()
            if (!removed.has(key(path))) {
                try {
                    for (const name of await base.list(path)) {
                        if (!removed.has(key(`${path}/${name}`))) names.add(name)
                    }
                } catch {
                    // base directory absent — overlay may still have it
                }
            }
            try {
                for (const name of await layer.list(path)) names.add(name)
            } catch {
                // overlay directory absent
            }
            return [...names].sort()
        },
        async rmdir(path, options) {
            removed.add(key(path))
            try {
                await layer.rmdir(path, options)
            } catch {
                // overlay never materialized this directory
            }
        },
        async readText(path) {
            if (await layer.exists(path)) return await layer.readText(path)
            return await base.readText(path)
        },
        async readBytes(path) {
            if (await layer.exists(path)) return await layer.readBytes(path)
            return await base.readBytes(path)
        },
        async writeText(path, data) {
            removed.delete(key(path))
            await layer.writeText(path, data)
        },
        async writeBytes(path, data) {
            removed.delete(key(path))
            await layer.writeBytes(path, data)
        },
        async appendText(path, data) {
            await copyUp(path)
            removed.delete(key(path))
            await layer.appendText(path, data)
        },
        async remove(path) {
            removed.add(key(path))
            try {
                await layer.remove(path)
            } catch {
                // not in the overlay
            }
        },
        async move(source, target) {
            await copyUp(source)
            await layer.move(source, target)
            removed.add(key(source))
            removed.delete(key(target))
        },
        async withSession(_path, body) {
            return await body()
        }
    }
}

const RECENTS_DB = 'fylo-explorer'
const RECENTS_STORE = 'roots'

/** @returns {Promise<IDBDatabase>} */
function openRecentsDb() {
    return new Promise((resolve, reject) => {
        const request = indexedDB.open(RECENTS_DB, 1)
        request.onupgradeneeded = () => request.result.createObjectStore(RECENTS_STORE)
        request.onsuccess = () => resolve(request.result)
        request.onerror = () => reject(request.error)
    })
}

/**
 * @param {'readonly' | 'readwrite'} mode
 * @param {(store: IDBObjectStore) => IDBRequest} body
 * @returns {Promise<any>}
 */
async function withRecents(mode, body) {
    const db = await openRecentsDb()
    try {
        return await new Promise((resolve, reject) => {
            const request = body(db.transaction(RECENTS_STORE, mode).objectStore(RECENTS_STORE))
            request.onsuccess = () => resolve(request.result)
            request.onerror = () => reject(request.error)
        })
    } finally {
        db.close()
    }
}

/**
 * Show the OS folder picker once and remember the handle, so later visits
 * reopen the root without a picker (Chromium can persist the permission via
 * "Allow on every visit"). The browser's one-time picker gesture is the web
 * equivalent of reading FYLO_ROOT from the environment.
 *
 * @param {{ mode?: 'read' | 'readwrite' }} [options]
 * @returns {Promise<FileSystemDirectoryHandle>}
 */
export async function pickFyloRoot(options = {}) {
    const picker = /** @type {{ showDirectoryPicker?: Function }} */ (globalThis)
        .showDirectoryPicker
    if (typeof picker !== 'function') {
        throw new Error('File System Access is not available in this browser (Chromium-only)')
    }
    const handle = await picker.call(globalThis, {
        id: 'fylo-root',
        mode: options.mode ?? 'read'
    })
    await withRecents('readwrite', (store) => store.put(handle, handle.name))
    return handle
}

/** @returns {Promise<FileSystemDirectoryHandle[]>} */
export async function listRecentRoots() {
    try {
        return (await withRecents('readonly', (store) => store.getAll())) ?? []
    } catch {
        return []
    }
}

/** @param {string} name @returns {Promise<void>} */
export async function forgetRecentRoot(name) {
    await withRecents('readwrite', (store) => store.delete(name))
}

/**
 * Re-arm a stored handle: resolves true when read access is (re)granted.
 * @param {FileSystemDirectoryHandle} handle
 * @param {{ mode?: 'read' | 'readwrite' }} [options]
 * @returns {Promise<boolean>}
 */
export async function ensureRootPermission(handle, options = {}) {
    const mode = options.mode ?? 'read'
    const query = /** @type {{ queryPermission?: Function, requestPermission?: Function }} */ (
        /** @type {unknown} */ (handle)
    )
    // Handles without the permission API (e.g. OPFS directories) are always usable.
    if (typeof query.queryPermission !== 'function') return true
    if ((await query.queryPermission.call(handle, { mode })) === 'granted') return true
    return (await query.requestPermission?.call(handle, { mode })) === 'granted'
}
