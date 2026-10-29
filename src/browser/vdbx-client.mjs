// The page side of a single-file database: pick it, open it in its Worker,
// make an editable copy, and save it back (ADR 0016 section 10).
//
// Browsers give a page a picked file to read but not to write in place, so a
// `.vdbx` opens read-only. "Edit a copy" streams it into the origin-private
// file system, where one synchronous access handle can write it, and "Save"
// writes the copy back: over the original through File System Access where
// the browser offers it, and as a download everywhere else.

import { WORKING_COPY_DIRECTORY, isWorkingCopyName } from './vdbx-names.mjs'

export { WORKING_COPY_DIRECTORY, isWorkingCopyName }

/** The `.vdbx` magic: `\x89VDBX\r\n\x1a`, PNG's pattern. */
export const VDBX_MAGIC = Object.freeze([0x89, 0x56, 0x44, 0x42, 0x58, 0x0d, 0x0a, 0x1a])
export const VDBX_MEDIA_TYPE = 'application/vnd.d31ma.vdbx'

const PICKER_TYPES = [{ description: 'FYLO database', accept: { [VDBX_MEDIA_TYPE]: ['.vdbx'] } }]

/**
 * File System Access members the DOM typings leave out.
 *
 * @typedef {{
 *   showOpenFilePicker?: (options: object) => Promise<FileSystemFileHandle[]>,
 *   showSaveFilePicker?: (options: object) => Promise<FileSystemFileHandle>
 * }} FilePickers
 * @typedef {FileSystemFileHandle & {
 *   createWritable?: () => Promise<{ write(data: Blob): Promise<void>, close(): Promise<void>, abort?(): Promise<void> }>,
 *   queryPermission?: (descriptor: { mode: string }) => Promise<string>,
 *   requestPermission?: (descriptor: { mode: string }) => Promise<string>
 * }} WritableFileHandle
 * @typedef {{ kind: 'file', file: Blob } | { kind: 'opfs', name: string, copyFrom?: Blob } | { kind: 'url', url: string }} DatabaseSource
 * @typedef {{ copied: number, total: number }} CopyProgress
 * @typedef {(message: Record<string, unknown>) => Promise<any>} WorkerCall
 */

/** One database file open in its own Worker. */
export class SingleFileDatabase {
    /**
     * @param {Worker} worker
     * @param {Record<string, unknown>} info what the Worker reported at open
     * @param {WorkerCall} call
     */
    constructor(worker, info, call) {
        this.worker = worker
        this.info = info
        this.call = call
    }

    /**
     * @param {DatabaseSource} source
     * @param {{ workerUrl: string | URL, moduleUrl?: string, path?: string, onProgress?: (progress: CopyProgress) => void }} options
     */
    static async open(source, { workerUrl, moduleUrl, path, onProgress }) {
        const worker = new Worker(workerUrl, { type: 'module' })
        const calls = createCalls(worker, onProgress)
        try {
            const info = await calls({ op: 'open', source, path, moduleUrl })
            return new SingleFileDatabase(worker, info, calls)
        } catch (error) {
            worker.terminate()
            throw error
        }
    }

    /** @returns {Promise<{ kind: number, len: number, modifiedMs: number }>} */
    stat() {
        return this.call({ op: 'host', call: 'stat' })
    }

    /** @param {number} offset @param {number} length @returns {Promise<Uint8Array>} */
    async read(offset, length) {
        return (await this.call({ op: 'host', call: 'read', offset, length })).bytes
    }

    /** @param {number} offset @param {Uint8Array} bytes @returns {Promise<{ written: number }>} */
    write(offset, bytes) {
        return this.call({ op: 'host', call: 'write', offset, bytes })
    }

    /** @param {number} length */
    truncate(length) {
        return this.call({ op: 'host', call: 'truncate', length })
    }

    flush() {
        return this.call({ op: 'host', call: 'flush' })
    }

    /** @param {string} ndjson @returns {Promise<string>} */
    async exec(ndjson) {
        return (await this.call({ op: 'exec', ndjson })).ndjson
    }

    /** The whole file as it stands, flushed first. @returns {Promise<Blob>} */
    async snapshot() {
        return (await this.call({ op: 'snapshot' })).blob
    }

    /** Close the file, release the working copy's lock and end the Worker. */
    async close() {
        try {
            await this.call({ op: 'close' })
        } finally {
            this.worker.terminate()
        }
    }
}

/**
 * @param {Worker} worker
 * @param {((progress: CopyProgress) => void) | undefined} onProgress
 * @returns {WorkerCall}
 */
function createCalls(worker, onProgress) {
    let next = 1
    /** @type {Map<number, { resolve: (value: any) => void, reject: (error: Error) => void }>} */
    const pending = new Map()
    worker.onmessage = (event) => {
        const { id, result, error, code, progress } = event.data ?? {}
        const waiting = pending.get(id)
        if (!waiting) return
        if (progress) {
            onProgress?.(progress)
            return
        }
        pending.delete(id)
        if (error) waiting.reject(Object.assign(new Error(error), { code }))
        else waiting.resolve(result)
    }
    worker.onerror = (event) => {
        const failure = new Error(event.message || 'the database Worker failed to start')
        for (const waiting of pending.values()) waiting.reject(failure)
        pending.clear()
    }
    return (message) =>
        new Promise((resolve, reject) => {
            const id = next++
            pending.set(id, { resolve, reject })
            worker.postMessage({ ...message, id })
        })
}

/** Whether the first bytes of a file carry the `.vdbx` magic. @param {ArrayLike<number>} bytes */
export function hasVdbxMagic(bytes) {
    return VDBX_MAGIC.every((value, index) => bytes[index] === value)
}

/**
 * Ask for a database with the native file picker, where the browser has one.
 *
 * Returns `null` when it does not (Firefox, Safari), and the caller falls back
 * to a file input. The handle is kept so Save can write over the original.
 *
 * @returns {Promise<{ file: File, handle: FileSystemFileHandle } | null>}
 */
export async function pickDatabaseFile() {
    const { showOpenFilePicker } = /** @type {FilePickers} */ (globalThis)
    if (typeof showOpenFilePicker !== 'function') return null
    const [handle] = await showOpenFilePicker({
        types: PICKER_TYPES,
        excludeAcceptAllOption: false,
        multiple: false
    })
    return { file: await handle.getFile(), handle }
}

/**
 * The file in a drop, with its handle where the browser offers one (Chromium).
 *
 * The handle has to be requested before the event returns; the data transfer
 * is emptied afterwards.
 *
 * @param {DataTransfer | null | undefined} transfer
 * @returns {Promise<{ file: File, handle?: FileSystemFileHandle } | null>}
 */
export async function databaseFromDrop(transfer) {
    const item = Array.from(transfer?.items ?? []).find((entry) => entry.kind === 'file')
    if (!item) return null
    const withHandle =
        /** @type {DataTransferItem & { getAsFileSystemHandle?: () => Promise<FileSystemHandle | null> }} */ (
            item
        )
    const pendingHandle =
        typeof withHandle.getAsFileSystemHandle === 'function'
            ? withHandle.getAsFileSystemHandle()
            : null
    const file = item.getAsFile()
    if (!file) return null
    const handle = await pendingHandle?.catch(() => null)
    return handle?.kind === 'file'
        ? { file, handle: /** @type {FileSystemFileHandle} */ (handle) }
        : { file }
}

/**
 * A working copy name for a picked file: its own name made safe, unique to now.
 *
 * @param {string} fileName
 * @param {number} [now]
 */
export function workingCopyName(fileName, now = Date.now()) {
    const base =
        String(fileName ?? '')
            .replace(/\.vdbx$/i, '')
            .replace(/[^A-Za-z0-9 ._()-]+/g, '-')
            .replace(/\.{2,}/g, '.')
            .replace(/^[^A-Za-z0-9]+/, '')
            .slice(0, 120) || 'database'
    return `${base}-${now.toString(36)}.vdbx`
}

/**
 * The working copies this origin holds, newest first.
 *
 * @returns {Promise<{ name: string, size: number, modifiedMs: number }[]>}
 */
export async function listWorkingCopies() {
    const directory = await workingCopyDirectory(false)
    if (!directory) return []
    const copies = []
    const listing =
        /** @type {FileSystemDirectoryHandle & { entries(): AsyncIterable<[string, FileSystemHandle]> }} */ (
            directory
        )
    for await (const [name, handle] of listing.entries()) {
        if (handle.kind !== 'file' || !isWorkingCopyName(name)) continue
        try {
            const file = await /** @type {FileSystemFileHandle} */ (handle).getFile()
            copies.push({ name, size: file.size, modifiedMs: file.lastModified })
        } catch {
            // Open in another tab's Worker; Safari refuses getFile then.
            copies.push({ name, size: 0, modifiedMs: 0 })
        }
    }
    return copies.sort((a, b) => b.modifiedMs - a.modifiedMs)
}

/** Delete one working copy. It must not be open. @param {string} name */
export async function removeWorkingCopy(name) {
    if (!isWorkingCopyName(name)) throw new Error(`invalid working copy name: ${name}`)
    const directory = await workingCopyDirectory(false)
    await directory?.removeEntry(name)
}

/** @param {boolean} create */
async function workingCopyDirectory(create) {
    const root = await navigator.storage.getDirectory()
    try {
        return await root.getDirectoryHandle(WORKING_COPY_DIRECTORY, { create })
    } catch (error) {
        if (/** @type {{ name?: string }} */ (error)?.name === 'NotFoundError') return null
        throw error
    }
}

/**
 * Write a database out.
 *
 * In order: over `handle` when one was kept and can be written; through a save
 * picker where the browser has one; otherwise as a download.
 *
 * @param {Blob} blob
 * @param {{ handle?: WritableFileHandle | null, suggestedName?: string, document?: Document }} [options]
 * @returns {Promise<'written' | 'downloaded'>}
 */
export async function saveDatabase(
    blob,
    { handle, suggestedName = 'database.vdbx', ...options } = {}
) {
    if (handle && typeof handle.createWritable === 'function' && (await canWrite(handle))) {
        await writeThrough(handle, blob)
        return 'written'
    }
    const { showSaveFilePicker } = /** @type {FilePickers} */ (globalThis)
    if (typeof showSaveFilePicker === 'function') {
        const target = await showSaveFilePicker({ suggestedName, types: PICKER_TYPES })
        await writeThrough(target, blob)
        return 'written'
    }
    downloadDatabase(blob, suggestedName, options.document)
    return 'downloaded'
}

/** @param {WritableFileHandle} handle */
async function canWrite(handle) {
    if (typeof handle.queryPermission !== 'function') return true
    const mode = { mode: 'readwrite' }
    if ((await handle.queryPermission(mode)) === 'granted') return true
    return (await handle.requestPermission?.(mode)) === 'granted'
}

/** `createWritable` writes a temporary file and swaps it in on close, so a failed save leaves the original. */
/** @param {WritableFileHandle} handle @param {Blob} blob */
async function writeThrough(handle, blob) {
    if (typeof handle.createWritable !== 'function') {
        throw new Error('this browser cannot write the file in place')
    }
    const writable = await handle.createWritable()
    try {
        await writable.write(blob)
        await writable.close()
    } catch (error) {
        await writable.abort?.().catch(() => {})
        throw error
    }
}

/** @param {Blob} blob @param {string} name @param {Document} [document] */
export function downloadDatabase(blob, name, document = globalThis.document) {
    const url = URL.createObjectURL(blob)
    const link = document.createElement('a')
    link.href = url
    link.download = name
    link.rel = 'noopener'
    link.style.display = 'none'
    document.body.append(link)
    link.click()
    link.remove()
    // Safari starts the download after the click returns.
    setTimeout(() => URL.revokeObjectURL(url), 60_000)
}
