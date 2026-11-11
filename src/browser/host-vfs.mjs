// The twelve filesystem functions `fylo-wasm` imports.
//
// The module owns the layout, the transactions, the index, and recovery. The
// host owns bytes and handles — nothing here knows what a document is.
//
// Attributes are part of the table rather than a file beside each record: the
// host decides where a record's manifest lives, so a browser root can keep one
// manifest instead of doubling its file count.
//
// `backend` supplies synchronous file operations. In a browser that is OPFS
// through `FileSystemSyncAccessHandle`, which is synchronous and therefore
// available only inside a dedicated Worker; `nodeBackend` below is the same
// shape over `node:fs` so the boundary can be tested without a browser.

export const HOST_ABI_VERSION = 2
export const MODULE_ABI_VERSION = 2

export const KIND_MISSING = 0
export const KIND_FILE = 1
export const KIND_DIRECTORY = 2

export const OPEN_READ = 1 << 0
export const OPEN_WRITE = 1 << 1
export const OPEN_CREATE = 1 << 2
export const OPEN_EXCLUSIVE = 1 << 3
export const OPEN_TRUNCATE = 1 << 4
export const OPEN_APPEND = 1 << 5

// Negative errno, matching what the module's `check` turns back into an error.
const ENOENT = -2
const EIO = -5
const EBADF = -9
const EACCES = -13
const EEXIST = -17
const EROFS = -30
const ESTALE = -116
const ENOSYS = -38

/**
 * Build the import object for one module instance.
 *
 * @param {{ memory: () => WebAssembly.Memory, backend: HostBackend }} options
 */
export function createHostImports({ memory, backend, identity, onError }) {
    const view = () => new Uint8Array(memory().buffer)
    const data = () => new DataView(memory().buffer)
    const decoder = new TextDecoder()
    const readPath = (pointer, length) => decoder.decode(view().subarray(pointer, pointer + length))

    // Every entry point is wrapped: a host that throws into WebAssembly
    // unwinds the module's stack mid-transaction, which is far worse than an
    // errno the engine can roll back from.
    const guard =
        (action) =>
        (...args) => {
            try {
                return action(...args)
            } catch (error) {
                // The engine only ever sees an errno, so the reason has to
                // leave by another door or a host bug looks like a disk fault.
                onError?.(error instanceof Error ? error.message : String(error))
                return errno(error)
            }
        }

    return {
        fylo_host: {
            open: guard((pathPointer, pathLength, flags, handlePointer) => {
                const handle = backend.open(readPath(pathPointer, pathLength), flags)
                data().setBigUint64(handlePointer, BigInt(handle), true)
                return 0
            }),
            close: guard((handle) => {
                backend.close(Number(handle))
                return 0
            }),
            read_at: guard((handle, offset, buffer, length, readPointer) => {
                const read = backend.readAt(
                    Number(handle),
                    Number(offset),
                    view().subarray(buffer, buffer + length)
                )
                data().setUint32(readPointer, read, true)
                return 0
            }),
            write_at: guard((handle, offset, buffer, length, writtenPointer) => {
                const written = backend.writeAt(
                    Number(handle),
                    Number(offset),
                    view().subarray(buffer, buffer + length)
                )
                data().setUint32(writtenPointer, written, true)
                return 0
            }),
            truncate: guard((handle, length) => {
                backend.truncate(Number(handle), Number(length))
                return 0
            }),
            flush: guard((handle) => {
                backend.flush(Number(handle))
                return 0
            }),
            stat: guard((pathPointer, pathLength, out) => {
                const entry = backend.stat(readPath(pathPointer, pathLength))
                const fields = data()
                fields.setUint32(out, entry.kind, true)
                // `len` is 8-byte aligned inside the repr(C) struct, so the
                // 4 bytes after `kind` are padding.
                fields.setBigUint64(out + 8, BigInt(entry.len ?? 0), true)
                fields.setBigUint64(out + 16, BigInt(entry.modifiedMs ?? 0), true)
                return 0
            }),
            mkdir: guard((pathPointer, pathLength, recursive) => {
                backend.mkdir(readPath(pathPointer, pathLength), recursive !== 0)
                return 0
            }),
            unlink: guard((pathPointer, pathLength) => {
                backend.unlink(readPath(pathPointer, pathLength))
                return 0
            }),
            rmdir: guard((pathPointer, pathLength, recursive) => {
                backend.rmdir(readPath(pathPointer, pathLength), recursive !== 0)
                return 0
            }),
            rename: guard((fromPointer, fromLength, toPointer, toLength) => {
                backend.rename(readPath(fromPointer, fromLength), readPath(toPointer, toLength))
                return 0
            }),
            read_attrs: guard((pathPointer, pathLength, buffer, capacity, neededPointer) => {
                const manifest = backend.readAttrs(readPath(pathPointer, pathLength))
                data().setUint32(neededPointer, manifest.length, true)
                if (manifest.length <= capacity) view().set(manifest, buffer)
                return 0
            }),
            write_attrs: guard((pathPointer, pathLength, manifest, length) => {
                backend.writeAttrs(
                    readPath(pathPointer, pathLength),
                    view().subarray(manifest, manifest + length)
                )
                return 0
            }),
            log: (pointer, length) => {
                backend.log?.(readPath(pointer, length))
            },
            now_unix_ms: guard((out) => {
                data().setBigUint64(out, BigInt(backend.nowUnixMs()), true)
                return 0
            }),
            random: guard((buffer, length) => {
                backend.random(view().subarray(buffer, buffer + length))
                return 0
            }),
            // ADR 0021 (decision P5-5): the application identity a protected
            // database opens with. `identity` answers synchronously, through
            // the crypto bridge Worker; without one every call is ENOSYS and
            // a protected database stays locked.
            identity_public: guard((out) => {
                if (!identity) return ENOSYS
                view().set(identity.publicKey(), out)
                return 0
            }),
            identity_sign: guard((message, length, out) => {
                if (!identity) return ENOSYS
                view().set(identity.sign(view().slice(message, message + length)), out)
                return 0
            }),
            identity_verify: guard((publicKey, message, length, signature) => {
                if (!identity) return ENOSYS
                const valid = identity.verify(
                    view().slice(publicKey, publicKey + 65),
                    view().slice(message, message + length),
                    view().slice(signature, signature + 64)
                )
                return valid ? 0 : 1
            }),
            identity_unwrap: guard((info, infoLength, wrapped, wrappedLength, out) => {
                if (!identity) return ENOSYS
                const key = identity.unwrap(
                    view().slice(info, info + infoLength),
                    view().slice(wrapped, wrapped + wrappedLength)
                )
                view().set(key, out)
                return 0
            }),
            identity_wrap: guard(
                (publicKey, info, infoLength, secret, secretLength, out, capacity, needed) => {
                    if (!identity) return ENOSYS
                    const wrapped = identity.wrap(
                        view().slice(publicKey, publicKey + 65),
                        view().slice(info, info + infoLength),
                        view().slice(secret, secret + secretLength)
                    )
                    data().setUint32(needed, wrapped.length, true)
                    if (wrapped.length <= capacity) view().set(wrapped, out)
                    return 0
                }
            ),
            read_dir: guard((pathPointer, pathLength, buffer, capacity, neededPointer) => {
                const names = backend.readDir(readPath(pathPointer, pathLength))
                const encoded = encodeNames(names)
                data().setUint32(neededPointer, encoded.length, true)
                // Over capacity the module retries with the reported length, so
                // writing a truncated listing would be a silent wrong answer.
                if (encoded.length <= capacity) view().set(encoded, buffer)
                return 0
            })
        }
    }
}

function encodeNames(names) {
    const encoder = new TextEncoder()
    const parts = names.map((name) => encoder.encode(`${name}\0`))
    const total = parts.reduce((sum, part) => sum + part.length, 0)
    const encoded = new Uint8Array(total)
    let offset = 0
    for (const part of parts) {
        encoded.set(part, offset)
        offset += part.length
    }
    return encoded
}

function errno(error) {
    switch (error?.code) {
        case 'ENOENT':
        case 'NotFoundError':
            return ENOENT
        case 'EEXIST':
            return EEXIST
        case 'EBADF':
            return EBADF
        case 'EACCES':
            return EACCES
        case 'EROFS':
            return EROFS
        case 'ESTALE':
            return ESTALE
        default:
            return EIO
    }
}

/**
 * Drive one machine batch through an instantiated module.
 *
 * @param {WebAssembly.Instance} instance
 * @param {string} ndjson newline-delimited request frames
 * @returns {string} newline-delimited response frames
 */
export function exec(instance, ndjson, trace = () => {}) {
    const { fylo_alloc, fylo_free, fylo_exec, fylo_abi_version, memory } = instance.exports
    if (fylo_abi_version() !== MODULE_ABI_VERSION) {
        throw new Error(`unsupported FYLO module ABI: ${fylo_abi_version()}`)
    }
    trace('abi-ok')
    const request = new TextEncoder().encode(ndjson)
    const requestPointer = fylo_alloc(request.length)
    trace('allocated')
    new Uint8Array(memory.buffer).set(request, requestPointer)
    trace('request-written')
    const packed = fylo_exec(requestPointer, request.length)
    trace('exec-returned')
    fylo_free(requestPointer, request.length)
    if (packed === 0n) return ''
    const pointer = Number(packed >> 32n)
    const length = Number(packed & 0xffffffffn)
    const response = new Uint8Array(memory.buffer).slice(pointer, pointer + length)
    fylo_free(pointer, length)
    return new TextDecoder().decode(response)
}

/**
 * Give the module its settings before its next batch (`fylo_configure`):
 * the `webcrypto:` identity a protected database opens with, and how its
 * creator is trusted. Throws `EIDENTITY` with the engine's reason when it
 * refuses them.
 *
 * @param {WebAssembly.Instance} instance
 * @param {{ identity?: string, trustAnchor?: string, trustOnFirstUse?: boolean }} settings
 */
export function configure(instance, settings) {
    const { fylo_configure, fylo_abi_version } = instance.exports
    if (fylo_abi_version() !== MODULE_ABI_VERSION) {
        throw new Error(`unsupported FYLO module ABI: ${fylo_abi_version()}`)
    }
    const request = new TextEncoder().encode(JSON.stringify(settings))
    sendSetting(instance, fylo_configure, request, 'EIDENTITY')
}

/**
 * Give the module the key of an encrypted `.vdbx` file
 * (`fylo_set_database_key`, ADR 0025): the bytes `VdbxKey.material` made.
 * They are wiped here and in module memory once the module has them. Throws
 * `EKEY` with the engine's reason when it refuses them.
 *
 * @param {WebAssembly.Instance} instance
 * @param {Uint8Array} material
 */
export function setDatabaseKey(instance, material) {
    try {
        sendSetting(instance, instance.exports.fylo_set_database_key, material, 'EKEY')
    } finally {
        material.fill(0)
    }
}

/**
 * Copy `bytes` into module memory, call `setting` with them, and throw the
 * reason it answers with, under `code`.
 *
 * @param {WebAssembly.Instance} instance
 * @param {(pointer: number, length: number) => bigint} setting
 * @param {Uint8Array} bytes
 * @param {string} code
 */
function sendSetting(instance, setting, bytes, code) {
    const { fylo_alloc, fylo_free, memory } = instance.exports
    const requestPointer = fylo_alloc(bytes.length)
    new Uint8Array(memory.buffer).set(bytes, requestPointer)
    const packed = setting(requestPointer, bytes.length)
    fylo_free(requestPointer, bytes.length)
    if (packed === 0n) return
    const pointer = Number(packed >> 32n)
    const length = Number(packed & 0xffffffffn)
    const reason = new TextDecoder().decode(
        new Uint8Array(memory.buffer).slice(pointer, pointer + length)
    )
    fylo_free(pointer, length)
    throw Object.assign(new Error(reason), { code })
}

/**
 * @typedef {object} HostBackend
 * @property {(path: string, flags: number) => number} open
 * @property {(handle: number) => void} close
 * @property {(handle: number, offset: number, into: Uint8Array) => number} readAt
 * @property {(handle: number, offset: number, from: Uint8Array) => number} writeAt
 * @property {(handle: number, length: number) => void} truncate
 * @property {(handle: number) => void} flush
 * @property {(path: string) => { kind: number, len?: number, modifiedMs?: number }} stat
 * @property {(path: string, recursive: boolean) => void} mkdir
 * @property {(path: string) => void} unlink
 * @property {(path: string, recursive: boolean) => void} rmdir
 * @property {(from: string, to: string) => void} rename
 * @property {(path: string) => string[]} readDir
 * @property {(into: Uint8Array) => void} random
 * @property {() => number} nowUnixMs
 * @property {(path: string) => Uint8Array} readAttrs
 * @property {(path: string, manifest: Uint8Array) => void} writeAttrs
 * @property {((message: string) => void)=} log
 */

/**
 * The application identity a host offers the engine (ADR 0021), answered
 * synchronously; `identity-bridge.mjs` builds one over a crypto Worker.
 *
 * @typedef {object} HostIdentity
 * @property {() => Uint8Array} publicKey 65-byte SEC1 uncompressed point
 * @property {(message: Uint8Array) => Uint8Array} sign 64-byte ECDSA P-256 signature
 * @property {(publicKey: Uint8Array, message: Uint8Array, signature: Uint8Array) => boolean} verify
 * @property {(info: Uint8Array, wrapped: Uint8Array) => Uint8Array} unwrap 32-byte key
 * @property {(publicKey: Uint8Array, info: Uint8Array, secret: Uint8Array) => Uint8Array} wrap
 */
