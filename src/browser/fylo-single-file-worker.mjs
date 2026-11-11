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
// - `{ kind: 'indexeddb', name, copyFrom? }`: the same working copy kept in
//   IndexedDB pages, for pages with no OPFS (ADR 0022). Every call that
//   changes the file is stored in one IndexedDB transaction before it is
//   answered.
// - `{ kind: 'local', name, copyFrom? }`: OPFS where this Worker has it,
//   IndexedDB otherwise. The open result names the one used in `storage`.
// - `{ kind: 'url', url }`: a hosted file, read-only, by ranged requests.
//
// Protocol: post `{ id, op, ... }`, receive `{ id, result }` or `{ id, error }`,
// plus `{ id, progress }` while a copy streams. Ops:
// - `open { source, path?, moduleUrl?, identity? }` opens the file, then
//   instantiates `fylo-browser.wasm` over it when `moduleUrl` is given.
//   `identity: { name, workerUrl, trustAnchor?, trustOnFirstUse? }` opens a
//   protected database as the origin's `webcrypto:<name>` identity (ADR
//   0021): `workerUrl` is the crypto bridge Worker
//   (`dist-web/identity-worker.js`), which answers the engine's synchronous
//   signing and unwrapping through a `SharedArrayBuffer`, so this one option
//   needs a cross-origin isolated page;
// - `exec { ndjson }` runs one engine batch;
// - `host { call, ... }` calls one `fylo_host` import directly (stat, read,
//   write, truncate, flush), through the same table the engine uses;
// - `snapshot` flushes and returns the whole file as a `Blob`, for Save;
// - `close` closes the file and releases the lock.

import {
    KIND_FILE,
    OPEN_READ,
    OPEN_WRITE,
    configure,
    createHostImports,
    exec,
    setDatabaseKey
} from './host-vfs.mjs'
import { VdbxKey } from './vdbx-key.mjs'
import {
    createIdentityBridgeBackend,
    createIdentityBridgeBuffer,
    describeIdentity
} from './identity-bridge.mjs'
import { IndexedDbFile } from './indexed-db-file.mjs'
import { WORKING_COPY_DIRECTORY, isWorkingCopyName } from './vdbx-names.mjs'
import {
    DEFAULT_DATABASE_PATH,
    createAccessHandleBackend,
    createFileBackend,
    createRangeBackend,
    createSingleFileBackend
} from './single-file-vfs.mjs'

const COPY_CHUNK_BYTES = 4 * 1024 * 1024
const HANDLE_WAIT_MS = 5000
const HANDLE_RETRY_MS = 100
const CREATE_PROBE_COLLECTION = 'fylo-create'

/** @type {ReturnType<typeof createFileBackend> | undefined} */
let backend
/** @type {FileSystemSyncAccessHandle | undefined} */
let accessHandle
/** @type {IndexedDbFile | undefined} */
let indexedFile
/** @type {'opfs' | 'indexeddb' | 'file' | 'url' | undefined} */
let storage
/** @type {(() => void) | undefined} */
let releaseLock
/** @type {WebAssembly.Instance | undefined} */
let instance
/** @type {ReturnType<typeof createProbe> | undefined} */
let probe
/** @type {string | undefined} */
let lastHostError
/** @type {Worker | undefined} */
let identityWorker
/** @type {string | null} */
let identityId = null

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
            return await stored(() => ({
                ndjson: exec(/** @type {WebAssembly.Instance} */ (instance), message.ndjson)
            }))
        case 'host':
            return await stored(() => opened().call(message))
        case 'snapshot':
            indexedFile?.refuseWhenFailed()
            return snapshot()
        case 'close':
            await close()
            return { closed: true }
        default:
            throw new Error(`unknown op: ${message.op}`)
    }
}

async function open({
    id,
    source,
    path = DEFAULT_DATABASE_PATH,
    moduleUrl,
    identity,
    encryption,
    create = false
}) {
    if (backend) throw new Error('this Worker already has a database open')
    lastHostError = undefined
    try {
        requireSupportedOpen(source, { moduleUrl, identity, encryption, create })
        if (source?.kind === 'file') {
            backend = createFileBackend(source.file, { path })
        } else if (source?.kind === 'url') {
            backend = createRangeBackend(new URL(source.url, self.location.href).href, { path })
        } else if (source?.kind === 'opfs') {
            await openWorkingCopy(
                source,
                path,
                (progress) => self.postMessage({ id, progress }),
                create
            )
        } else if (source?.kind === 'indexeddb') {
            await openIndexedCopy(source, path, (progress) => self.postMessage({ id, progress }))
        } else if (source?.kind === 'local') {
            const onProgress = (progress) => self.postMessage({ id, progress })
            if (await hasOpfs()) await openWorkingCopy(source, path, onProgress, false)
            else await openIndexedCopy(source, path, onProgress)
        } else {
            throw new Error(`unknown database source: ${source?.kind}`)
        }
        const key = encryption ? await VdbxKey.material(encryption, backend, create) : null
        if (moduleUrl) instance = await instantiate(moduleUrl, backend, identity, key)
        if (create) createIfMissing(path)
        probe = createProbe(backend)
        storage = storage ?? source.kind
        return describe()
    } catch (error) {
        await close()
        throw error
    }
}

/**
 * Run one call, then store what it changed before it is answered. A failed
 * store turns the answer into that failure: the change is not stored, so it
 * must not be reported as done.
 *
 * @template T
 * @param {() => T} action
 * @returns {Promise<T>}
 */
async function stored(action) {
    indexedFile?.refuseWhenFailed()
    try {
        return action()
    } finally {
        await indexedFile?.commit()
    }
}

/** Whether this Worker can keep a working copy in OPFS. */
async function hasOpfs() {
    if (typeof navigator.storage?.getDirectory !== 'function') return false
    if (typeof FileSystemFileHandle?.prototype?.createSyncAccessHandle !== 'function') return false
    try {
        await navigator.storage.getDirectory()
        return true
    } catch {
        return false
    }
}

async function openIndexedCopy({ name, copyFrom }, path, onProgress) {
    if (!isWorkingCopyName(name)) throw new Error(`invalid working copy name: ${name}`)
    indexedFile = await IndexedDbFile.open(name, { copyFrom, onProgress })
    const file = indexedFile
    storage = 'indexeddb'
    backend = createSingleFileBackend({
        path,
        writable: true,
        size: () => file.size(),
        modifiedMs: () => file.modifiedMs(),
        readAt: (offset, into) => file.readAt(offset, into),
        writeAt: (offset, from) => file.writeAt(offset, from),
        truncate: (length) => file.truncate(length)
    })
}

/**
 * Refuse what the browser engine does not do before anything is opened: a
 * key together with an identity, and creating a database that is not
 * encrypted, not an OPFS working copy, or without an engine (ADR 0025).
 */
function requireSupportedOpen(source, { moduleUrl, identity, encryption, create }) {
    if (encryption && identity) {
        throw unsupported('a protected database opens with an identity, not with encryption.key')
    }
    if (!create) return
    if (!encryption) {
        throw unsupported('the browser creates only encrypted databases: pass encryption.key')
    }
    if (source?.kind !== 'opfs' || source.copyFrom || !moduleUrl) {
        throw unsupported(
            'create needs an OPFS working copy by name, with no copyFrom, and moduleUrl'
        )
    }
}

/** @param {string} message */
function unsupported(message) {
    return Object.assign(new Error(message), { code: 'EBROWSER_UNSUPPORTED' })
}

/**
 * Let the engine create the database at `path` if the working copy is new:
 * the first request that names a missing root creates it, encrypted with
 * the key the page gave.
 */
function createIfMissing(path) {
    if (backend.stat(path).kind === KIND_FILE) return
    const request = { op: 'findDocs', root: path, collection: CREATE_PROBE_COLLECTION, query: {} }
    const [frame] = exec(instance, `${JSON.stringify(request)}\n`)
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line))
    if (backend.stat(path).kind === KIND_FILE) return
    throw Object.assign(new Error(frame?.error?.message ?? 'the database was not created'), {
        code: frame?.error?.code ?? 'EUNKNOWN'
    })
}

async function instantiate(moduleUrl, target, identity, key) {
    let created
    const imports = createHostImports({
        memory: () => created.exports.memory,
        backend: target,
        identity: identity ? await startIdentityBridge(identity) : undefined,
        onError: (message) => {
            lastHostError = message
        }
    })
    created = (await WebAssembly.instantiateStreaming(fetch(moduleUrl), imports)).instance
    if (identity) {
        configure(created, {
            identity: `webcrypto:${identity.name}`,
            trustAnchor: identity.trustAnchor,
            trustOnFirstUse: identity.trustOnFirstUse === true
        })
    }
    if (key) setDatabaseKey(created, key)
    return created
}

/**
 * Start the crypto bridge Worker for `identity.name` and return the
 * synchronous identity the host table answers the engine with.
 */
async function startIdentityBridge({ name, workerUrl }) {
    if (typeof SharedArrayBuffer !== 'function' || globalThis.crossOriginIsolated !== true) {
        throw Object.assign(
            new Error(
                'opening a database as a webcrypto: identity needs a cross-origin isolated ' +
                    'page (COOP: same-origin, COEP: require-corp)'
            ),
            { code: 'ENOISOLATION' }
        )
    }
    const buffer = createIdentityBridgeBuffer()
    identityWorker = new Worker(workerUrl, { type: 'module' })
    const ready = await new Promise((resolve) => {
        identityWorker.onmessage = (event) => resolve(event.data ?? {})
        identityWorker.onerror = (event) =>
            resolve({ error: event.message || 'the crypto bridge Worker failed to start' })
        identityWorker.postMessage({ buffer, name })
    })
    if (!ready.ready) {
        throw Object.assign(new Error(ready.error), { code: ready.code ?? 'EIDENTITY' })
    }
    identityId = (await describeIdentity(ready.publicKey)).id
    return createIdentityBridgeBackend(buffer)
}

async function openWorkingCopy({ name, copyFrom }, path, progress, create) {
    if (!isWorkingCopyName(name)) throw new Error(`invalid working copy name: ${name}`)
    storage = 'opfs'
    await acquireLock(`fylo-vdbx:${name}`)
    const root = await navigator.storage.getDirectory()
    const directory = await root.getDirectoryHandle(WORKING_COPY_DIRECTORY, { create: true })
    const file = await directory.getFileHandle(name, { create: Boolean(copyFrom) || create })
    accessHandle = await createAccessHandle(file)
    if (copyFrom) await streamInto(accessHandle, copyFrom, progress)
    const absent = create && accessHandle.getSize() === 0
    backend = createAccessHandleBackend(accessHandle, { path, absent })
}

/**
 * Open the copy's sync access handle. Holding the Web Lock means no other
 * Worker owns the copy, but Firefox can release a killed Worker's lock a
 * moment before its handle, and refuses a new handle meanwhile; wait for it
 * a short while, then report the copy as busy so the caller can try again.
 *
 * @param {FileSystemFileHandle} file
 * @returns {Promise<FileSystemSyncAccessHandle>}
 */
async function createAccessHandle(file) {
    for (let waited = 0; ; waited += HANDLE_RETRY_MS) {
        try {
            return await file.createSyncAccessHandle()
        } catch (error) {
            if (error?.name !== 'NoModificationAllowedError') throw error
            if (waited >= HANDLE_WAIT_MS) {
                throw Object.assign(
                    new Error('this working copy is still held by a closing Worker'),
                    {
                        code: 'EBUSY'
                    }
                )
            }
        }
        await new Promise((resolve) => setTimeout(resolve, HANDLE_RETRY_MS))
    }
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
        storage,
        identity: identityId,
        crossOriginIsolated: globalThis.crossOriginIsolated === true
    }
}

function opened() {
    if (!probe) throw new Error('no database is open')
    return probe
}

async function close() {
    probe?.close()
    probe = undefined
    instance = undefined
    backend = undefined
    storage = undefined
    const closing = indexedFile
    indexedFile = undefined
    identityWorker?.terminate()
    identityWorker = undefined
    identityId = null
    try {
        accessHandle?.close()
    } finally {
        accessHandle = undefined
        releaseLock?.()
        releaseLock = undefined
        await closing?.close()
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
