// The Worker that owns one `.vdbx` database file in a browser (ADR 0016
// section 10).
//
// The sibling `fylo-wasm-worker.mjs` runs the engine over a folder, which
// needs asynchronous directory calls made synchronous through a second Worker
// and a `SharedArrayBuffer`, and so a cross-origin isolated page. A database
// in one file needs none of that: the backends in `single-file-vfs.mjs` are
// synchronous already, so this Worker runs on any static host.
//
// Sources:
// - `{ kind: 'file', file }`: a picked or dropped `File`, read-only.
// - `{ kind: 'opfs', name, copyFrom? }`: an OPFS working copy, read/write.
//   `copyFrom` streams a `Blob` into it first. The copy is held under a Web
//   Lock for the life of the Worker, so a second tab is refused instead of
//   corrupting it.
// - `{ kind: 'url', url }`: a hosted file, read-only, by ranged requests.
//
// Protocol: post `{ id, op, ... }`, receive `{ id, result }` or `{ id, error }`,
// plus `{ id, progress }` while a copy streams. Ops:
// - `open { source, path?, moduleUrl? }` opens the file, then instantiates
//   `fylo-browser.wasm` over it when `moduleUrl` is given;
// - `exec { ndjson }` runs one engine batch;
// - `host { call, ... }` calls one `fylo_host` import directly (stat, read,
//   write, truncate, flush), through the same table the engine uses;
// - `snapshot` flushes and returns the whole file as a `Blob`, for Save;
// - `close` closes the file and releases the lock.

import { OPEN_READ, OPEN_WRITE, createHostImports, exec } from './host-vfs.mjs'
import { WORKING_COPY_DIRECTORY, isWorkingCopyName } from './vdbx-names.mjs'
import {
    DEFAULT_DATABASE_PATH,
    createAccessHandleBackend,
    createFileBackend,
    createRangeBackend
} from './single-file-vfs.mjs'

const COPY_CHUNK_BYTES = 4 * 1024 * 1024

/** @type {ReturnType<typeof createFileBackend> | undefined} */
let backend
/** @type {FileSystemSyncAccessHandle | undefined} */
let accessHandle
/** @type {(() => void) | undefined} */
let releaseLock
/** @type {WebAssembly.Instance | undefined} */
let instance
/** @type {ReturnType<typeof createProbe> | undefined} */
let probe
/** @type {string | undefined} */
let lastHostError

// Calls are answered in order: an `open` still streaming a copy must finish
// before an `exec` behind it touches the file.
let queue = Promise.resolve()

self.onmessage = (event) => {
    const message = event.data ?? {}
    queue = queue.then(() => handle(message))
}

async function handle(message) {
    const { id } = message
    try {
        const result = await dispatch(message)
        const transfer = result?.bytes instanceof Uint8Array ? [result.bytes.buffer] : []
        self.postMessage({ id, result, hostError: lastHostError }, transfer)
    } catch (error) {
        self.postMessage({
            id,
            error: error instanceof Error ? error.message : String(error),
            code: error?.code ?? error?.name,
            hostError: lastHostError
        })
    }
}

async function dispatch(message) {
    switch (message.op) {
        case 'open':
            return await open(message)
        case 'exec':
            if (!instance) throw new Error('the engine was not loaded; pass moduleUrl to open')
            return { ndjson: exec(instance, message.ndjson) }
        case 'host':
            return opened().call(message)
        case 'snapshot':
            return snapshot()
        case 'close':
            close()
            return { closed: true }
        default:
            throw new Error(`unknown op: ${message.op}`)
    }
}

async function open({ id, source, path = DEFAULT_DATABASE_PATH, moduleUrl }) {
    if (backend) throw new Error('this Worker already has a database open')
    lastHostError = undefined
    try {
        if (source?.kind === 'file') {
            backend = createFileBackend(source.file, { path })
        } else if (source?.kind === 'url') {
            backend = createRangeBackend(new URL(source.url, self.location.href).href, { path })
        } else if (source?.kind === 'opfs') {
            await openWorkingCopy(source, path, (progress) => self.postMessage({ id, progress }))
        } else {
            throw new Error(`unknown database source: ${source?.kind}`)
        }
        probe = createProbe(backend)
        if (moduleUrl) instance = await instantiate(moduleUrl, backend)
        return describe()
    } catch (error) {
        close()
        throw error
    }
}

async function instantiate(moduleUrl, target) {
    let created
    const imports = createHostImports({
        memory: () => created.exports.memory,
        backend: target,
        onError: (message) => {
            lastHostError = message
        }
    })
    created = (await WebAssembly.instantiateStreaming(fetch(moduleUrl), imports)).instance
    return created
}

async function openWorkingCopy({ name, copyFrom }, path, progress) {
    if (!isWorkingCopyName(name)) throw new Error(`invalid working copy name: ${name}`)
    await acquireLock(`fylo-vdbx:${name}`)
    const root = await navigator.storage.getDirectory()
    const directory = await root.getDirectoryHandle(WORKING_COPY_DIRECTORY, { create: true })
    const file = await directory.getFileHandle(name, { create: Boolean(copyFrom) })
    accessHandle = await file.createSyncAccessHandle()
    if (copyFrom) await streamInto(accessHandle, copyFrom, progress)
    backend = createAccessHandleBackend(accessHandle, { path })
}

/** Copy a `Blob` into the handle a chunk at a time, so a large file never sits in memory. */
async function streamInto(handle, blob, progress) {
    handle.truncate(0)
    const total = blob.size
    let copied = 0
    const reader = blob.stream().getReader()
    for (;;) {
        const { done, value } = await reader.read()
        if (done) break
        let written = 0
        while (written < value.length) {
            written += handle.write(value.subarray(written), { at: copied + written })
        }
        copied += value.length
        progress({ copied, total })
    }
    if (copied !== total) throw new Error(`copied ${copied} of ${total} bytes`)
    handle.flush()
}

function snapshot() {
    const file = opened()
    if (file.backend.writable) accessHandle?.flush()
    const size = file.backend.size()
    const parts = []
    for (let offset = 0; offset < size; offset += COPY_CHUNK_BYTES) {
        const chunk = new Uint8Array(Math.min(COPY_CHUNK_BYTES, size - offset))
        let read = 0
        while (read < chunk.length) {
            const count = file.backend.readAt(file.handle, offset + read, chunk.subarray(read))
            if (count === 0) throw new Error(`the file ended at ${offset + read} of ${size} bytes`)
            read += count
        }
        parts.push(chunk)
    }
    return { blob: new Blob(parts, { type: 'application/vnd.d31ma.vdbx' }), size }
}

function describe() {
    const { kind, len, modifiedMs } = opened().call({ call: 'stat' })
    return {
        path: backend?.path,
        writable: backend?.writable ?? false,
        kind,
        size: len,
        modifiedMs,
        engine: Boolean(instance),
        crossOriginIsolated: globalThis.crossOriginIsolated === true
    }
}

function opened() {
    if (!probe) throw new Error('no database is open')
    return probe
}

function close() {
    probe?.close()
    probe = undefined
    instance = undefined
    backend = undefined
    try {
        accessHandle?.close()
    } finally {
        accessHandle = undefined
        releaseLock?.()
        releaseLock = undefined
    }
}

/**
 * Take the working copy's Web Lock, or fail at once if another tab holds it.
 *
 * Waiting would leave this tab hung on a copy someone else is editing; saying
 * so lets the person close the other tab. The lock is released by `close`, or
 * by the browser when the Worker or the page goes away.
 */
async function acquireLock(name) {
    if (!globalThis.navigator?.locks) return
    await new Promise((granted, failed) => {
        navigator.locks
            .request(name, { mode: 'exclusive', ifAvailable: true }, (lock) => {
                if (!lock) {
                    failed(
                        Object.assign(
                            new Error('this working copy is open in another tab or window'),
                            { code: 'EBUSY' }
                        )
                    )
                    return undefined
                }
                granted(undefined)
                return new Promise((release) => {
                    releaseLock = release
                })
            })
            .catch(failed)
    })
}

/**
 * Drive the `fylo_host` table without the engine, through a scratch memory.
 *
 * The table reads paths from and writes results into module memory, so these
 * calls exercise exactly the pointers, lengths and errno the engine's own
 * calls do. It is how the browser tests prove the backends before the `.vdbx`
 * engine ops exist, and how Explorer reads the header today.
 */
function createProbe(target) {
    const memory = new WebAssembly.Memory({ initial: 1 })
    const table = createHostImports({
        memory: () => memory,
        backend: target,
        onError: (message) => {
            lastHostError = message
        }
    }).fylo_host
    const SCRATCH = 0
    const PATH = 64
    const DATA = 4096
    const encoded = new TextEncoder().encode(target.path)
    const view = () => new DataView(memory.buffer)
    const bytes = () => new Uint8Array(memory.buffer)
    const room = (length) => {
        const needed = DATA + length - memory.buffer.byteLength
        if (needed > 0) memory.grow(Math.ceil(needed / 65536))
    }
    const check = (code, what) => {
        if (code !== 0) {
            throw Object.assign(new Error(`${what} failed with errno ${-code}: ${lastHostError}`), {
                code: `E${-code}`
            })
        }
    }
    const openHandle = (flags) => {
        bytes().set(encoded, PATH)
        check(table.open(PATH, encoded.length, flags, SCRATCH), 'open')
        return view().getBigUint64(SCRATCH, true)
    }
    const handle = openHandle(target.writable ? OPEN_READ | OPEN_WRITE : OPEN_READ)

    return {
        backend: target,
        handle: Number(handle),
        call(request) {
            switch (request.call) {
                case 'stat': {
                    bytes().set(encoded, PATH)
                    check(table.stat(PATH, encoded.length, SCRATCH), 'stat')
                    return {
                        kind: view().getUint32(SCRATCH, true),
                        len: Number(view().getBigUint64(SCRATCH + 8, true)),
                        modifiedMs: Number(view().getBigUint64(SCRATCH + 16, true))
                    }
                }
                case 'read': {
                    room(request.length)
                    check(
                        table.read_at(
                            handle,
                            BigInt(request.offset),
                            DATA,
                            request.length,
                            SCRATCH
                        ),
                        'read_at'
                    )
                    const read = view().getUint32(SCRATCH, true)
                    return { read, bytes: bytes().slice(DATA, DATA + read) }
                }
                case 'write': {
                    const from = new Uint8Array(request.bytes)
                    room(from.length)
                    bytes().set(from, DATA)
                    check(
                        table.write_at(handle, BigInt(request.offset), DATA, from.length, SCRATCH),
                        'write_at'
                    )
                    return { written: view().getUint32(SCRATCH, true) }
                }
                case 'truncate':
                    check(table.truncate(handle, BigInt(request.length)), 'truncate')
                    return { length: request.length }
                case 'flush':
                    check(table.flush(handle), 'flush')
                    return { flushed: true }
                case 'openWrite':
                    // Asking for a writable handle on a read-only source must
                    // fail in the table, not only in this probe.
                    table.close(openHandle(OPEN_READ | OPEN_WRITE))
                    return { opened: true }
                default:
                    throw new Error(`unknown host call: ${request.call}`)
            }
        },
        close() {
            table.close(handle)
        }
    }
}
