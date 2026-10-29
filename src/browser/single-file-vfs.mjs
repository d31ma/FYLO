// Host backends for a database held in one file (ADR 0016 section 10).
//
// A `.vdbx` engine needs six operations on a single file — open, stat,
// read_at, write_at, truncate and flush — and every one of them can be served
// synchronously inside a dedicated Worker without the `SharedArrayBuffer`
// bridge a folder root needs:
//
// - a picked or dropped `File` is read with `FileReaderSync`;
// - an OPFS working copy is one `FileSystemSyncAccessHandle`;
// - a hosted file is read with synchronous ranged `XMLHttpRequest`.
//
// So none of them asks for cross-origin isolation. Each backend has the shape
// `createHostImports` expects and exposes exactly one path, the database file;
// the directory that holds it is listed as containing only that file, and
// every other path is missing. Directory changes are refused, because a
// container never needs them.

import { KIND_DIRECTORY, KIND_FILE, KIND_MISSING, OPEN_CREATE, OPEN_WRITE } from './host-vfs.mjs'

/** Where the engine finds the database unless the caller names another path. */
export const DEFAULT_DATABASE_PATH = '/database.vdbx'

/**
 * Read-only backend over a `File` or `Blob`.
 *
 * A picked file is a snapshot: browsers refuse to read a `File` whose bytes
 * changed on disk after it was picked (`NotReadableError`), so a change
 * surfaces as `ESTALE` rather than as torn reads.
 *
 * @param {Blob} file
 * @param {{ path?: string, reader?: { readAsArrayBuffer(blob: Blob): ArrayBuffer } }} [options]
 */
export function createFileBackend(file, { path = DEFAULT_DATABASE_PATH, reader } = {}) {
    const sync = reader ?? createReaderSync()
    const modifiedMs = /** @type {File} */ (file).lastModified ?? 0
    return createSingleFileBackend({
        path,
        writable: false,
        size: () => file.size,
        modifiedMs: () => modifiedMs,
        readAt(offset, into) {
            const end = Math.min(offset + into.length, file.size)
            if (end <= offset) return 0
            let bytes
            try {
                bytes = sync.readAsArrayBuffer(file.slice(offset, end))
            } catch (error) {
                throw stale(error)
            }
            into.set(new Uint8Array(bytes))
            return bytes.byteLength
        }
    })
}

/**
 * Read/write backend over one OPFS `FileSystemSyncAccessHandle`.
 *
 * The handle is exclusive by construction: a second `createSyncAccessHandle`
 * on the same file fails while this one is open, in this tab or another.
 *
 * @param {FileSystemSyncAccessHandle} handle
 * @param {{ path?: string, writable?: boolean }} [options]
 */
export function createAccessHandleBackend(
    handle,
    { path = DEFAULT_DATABASE_PATH, writable = true } = {}
) {
    // A sync access handle has no modification time; the engine keeps its own.
    let modifiedMs = Date.now()
    return createSingleFileBackend({
        path,
        writable,
        size: () => handle.getSize(),
        modifiedMs: () => modifiedMs,
        readAt: (offset, into) => handle.read(into, { at: offset }),
        writeAt(offset, from) {
            const written = handle.write(from, { at: offset })
            modifiedMs = Date.now()
            return written
        },
        truncate(length) {
            handle.truncate(length)
            modifiedMs = Date.now()
        },
        flush: () => handle.flush()
    })
}

/**
 * Read-only backend over a URL, one synchronous ranged request per read.
 *
 * The server must answer `Range` with `206 Partial Content`; one that sends
 * the whole body instead would turn every read into a full download, so it
 * is refused. The first response's validator is sent back as `If-Range`, so a
 * file replaced on the server answers `200` and surfaces as `ESTALE` instead
 * of mixing bytes from two versions.
 *
 * @param {string} url
 * @param {{ path?: string, XMLHttpRequest?: typeof XMLHttpRequest }} [options]
 */
export function createRangeBackend(url, { path = DEFAULT_DATABASE_PATH, ...options } = {}) {
    const Request = options.XMLHttpRequest ?? globalThis.XMLHttpRequest
    let validator = ''
    let modifiedMs = 0

    function fetchRange(first, last) {
        const request = new Request()
        request.open('GET', url, false)
        request.responseType = 'arraybuffer'
        request.setRequestHeader('Range', `bytes=${first}-${last}`)
        if (validator) request.setRequestHeader('If-Range', validator)
        request.send()
        if (request.status === 416) return { total: null, bytes: new Uint8Array() }
        if (request.status === 200) {
            throw validator
                ? withCode(new Error(`${url} changed on the server since it was opened`), 'ESTALE')
                : withCode(new Error(`${url} does not support byte-range requests`), 'EIO')
        }
        if (request.status !== 206) {
            throw withCode(
                new Error(`range request for ${url} failed with HTTP ${request.status}`),
                request.status === 404 ? 'ENOENT' : 'EIO'
            )
        }
        const range = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(
            request.getResponseHeader('Content-Range') ?? ''
        )
        if (!range || Number(range[1]) !== first) {
            throw withCode(new Error(`${url} answered a range it was not asked for`), 'EIO')
        }
        return {
            total: Number(range[3]),
            bytes: new Uint8Array(request.response),
            validator:
                strongEtag(request.getResponseHeader('ETag')) ||
                request.getResponseHeader('Last-Modified') ||
                '',
            modified: Date.parse(request.getResponseHeader('Last-Modified') ?? '')
        }
    }

    // One byte answers the size, the validator and whether ranges work at all.
    const probe = fetchRange(0, 0)
    if (probe.total === null) throw withCode(new Error(`${url} is empty`), 'EIO')
    const size = probe.total
    validator = probe.validator ?? ''
    modifiedMs = Number.isFinite(probe.modified) ? probe.modified : 0

    return createSingleFileBackend({
        path,
        writable: false,
        size: () => size,
        modifiedMs: () => modifiedMs,
        readAt(offset, into) {
            const end = Math.min(offset + into.length, size)
            if (end <= offset) return 0
            const { bytes } = fetchRange(offset, end - 1)
            if (bytes.length !== end - offset) {
                throw withCode(
                    new Error(`${url} returned ${bytes.length} of ${end - offset} bytes`),
                    'EIO'
                )
            }
            into.set(bytes)
            return bytes.length
        }
    })
}

/**
 * The shared shape: one file, handles onto it, and refusals for the rest.
 *
 * @param {{
 *   path: string,
 *   writable: boolean,
 *   size: () => number,
 *   modifiedMs: () => number,
 *   readAt: (offset: number, into: Uint8Array) => number,
 *   writeAt?: (offset: number, from: Uint8Array) => number,
 *   truncate?: (length: number) => void,
 *   flush?: () => void
 * }} file
 * @returns {import('./host-vfs.mjs').HostBackend & { path: string, writable: boolean, size: () => number }}
 */
export function createSingleFileBackend(file) {
    const path = normalize(file.path)
    const parent = path.slice(0, path.lastIndexOf('/')) || '/'
    const name = path.slice(path.lastIndexOf('/') + 1)
    /** @type {Map<number, { write: boolean }>} */
    const handles = new Map()
    let nextHandle = 1

    const handleOf = (handle) => {
        const open = handles.get(handle)
        if (!open) throw withCode(new Error(`handle ${handle} is not open`), 'EBADF')
        return open
    }
    /** @returns {never} */
    const refuse = (what) => {
        throw withCode(
            new Error(`${what} is refused: this host holds the single file ${path}`),
            file.writable ? 'EACCES' : 'EROFS'
        )
    }

    return {
        path,
        writable: file.writable,
        size: file.size,
        open(requested, flags) {
            if (normalize(requested) !== path) {
                if (flags & OPEN_CREATE) refuse(`creating ${requested}`)
                throw withCode(new Error(`${requested} does not exist`), 'ENOENT')
            }
            const write = (flags & OPEN_WRITE) !== 0
            if (write && !file.writable) refuse(`opening ${path} for writing`)
            const handle = nextHandle++
            handles.set(handle, { write })
            return handle
        },
        close(handle) {
            handleOf(handle)
            handles.delete(handle)
        },
        readAt(handle, offset, into) {
            handleOf(handle)
            return file.readAt(offset, into)
        },
        writeAt(handle, offset, from) {
            const { writeAt } = file
            if (!handleOf(handle).write || !writeAt) return refuse(`writing ${path}`)
            return writeAt(offset, from)
        },
        truncate(handle, length) {
            const { truncate } = file
            if (!handleOf(handle).write || !truncate) return refuse(`truncating ${path}`)
            truncate(length)
        },
        flush(handle) {
            handleOf(handle)
            file.flush?.()
        },
        stat(requested) {
            const at = normalize(requested)
            if (at === path) {
                return { kind: KIND_FILE, len: file.size(), modifiedMs: file.modifiedMs() }
            }
            if (at === parent) return { kind: KIND_DIRECTORY, len: 0, modifiedMs: 0 }
            return { kind: KIND_MISSING }
        },
        readDir(requested) {
            if (normalize(requested) === parent) return [name]
            throw withCode(new Error(`${requested} is not a directory`), 'ENOENT')
        },
        mkdir: (requested) => refuse(`creating directory ${requested}`),
        unlink: (requested) => refuse(`removing ${requested}`),
        rmdir: (requested) => refuse(`removing directory ${requested}`),
        rename: (from) => refuse(`renaming ${from}`),
        // A container keeps its own metadata; the file itself has none.
        readAttrs(requested) {
            if (normalize(requested) !== path) {
                throw withCode(new Error(`${requested} does not exist`), 'ENOENT')
            }
            return new Uint8Array()
        },
        writeAttrs: (requested) => refuse(`writing attributes of ${requested}`),
        random: (into) => crypto.getRandomValues(into),
        nowUnixMs: () => Date.now(),
        log: (message) => console.error(message)
    }
}

function createReaderSync() {
    if (typeof FileReaderSync !== 'function') {
        throw new Error('FileReaderSync is unavailable; open the database inside a Worker')
    }
    return new FileReaderSync()
}

function normalize(path) {
    if (typeof path !== 'string' || !path.startsWith('/')) {
        throw withCode(new Error('single-file host paths must be absolute'), 'EACCES')
    }
    const parts = []
    for (const part of path.split('/')) {
        if (part === '' || part === '.') continue
        if (part === '..') {
            throw withCode(new Error(`${path} leaves the database directory`), 'EACCES')
        }
        parts.push(part)
    }
    return `/${parts.join('/')}`
}

/** Weak validators cannot be used with `If-Range`. */
function strongEtag(etag) {
    return etag && !etag.startsWith('W/') ? etag : ''
}

function stale(error) {
    if (error?.name === 'NotReadableError') {
        return withCode(
            new Error('the file changed on disk since it was opened; open it again'),
            'ESTALE'
        )
    }
    return error
}

function withCode(error, code) {
    return Object.assign(error, { code })
}
