import { dlopen, FFIType, ptr, read } from 'bun:ffi'
import os from 'node:os'
import {
    closeSync,
    fsyncSync,
    lstatSync,
    openSync,
    readFileSync,
    statSync,
    unlinkSync,
    writeFileSync
} from 'node:fs'
import {
    windowsDeleteNamedStream,
    windowsReadNamedStream,
    windowsTryAcquireProcessFileLock,
    windowsWithDescriptorLock,
    windowsWriteNamedStream
} from './windows-secure-open.js'

/**
 * Extended-attribute access via libc, bound with bun:ffi.
 *
 * No native addon: xattr syscall wrappers already live in libc, so we
 * dlopen it directly. On Linux, attribute names must use the `user.`
 * namespace (e.g. `user.fylo.checksum`); macOS accepts any name.
 */

const darwin = process.platform === 'darwin'
const windows = process.platform === 'win32'

/**
 * The darwin/linux symbol maps differ in arity, so tsc collapses the ternary
 * union to `never`; normalize to one loose signature per symbol instead.
 * @typedef {{
 *   setxattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number,
 *   getxattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number | bigint,
 *   listxattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number | bigint,
 *   removexattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number,
 *   lsetxattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number,
 *   lgetxattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number | bigint,
 *   llistxattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number | bigint,
 *   lremovexattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number,
 *   fgetxattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number | bigint,
 *   flistxattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number | bigint,
 *   fsetxattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number,
 *   fremovexattr?: (...args: (number | import('bun:ffi').Pointer | null)[]) => number,
 *   __error?: () => import('bun:ffi').Pointer,
 *   __errno_location?: () => import('bun:ffi').Pointer,
 *   strerror: (code: number) => string,
 * }} XattrSymbols
 */

// ponytail: darwin + glibc linux only; add musl/BSD library names if a target ever needs them
const libc = /** @type {XattrSymbols | null} */ (
    /** @type {unknown} */ (
        windows
            ? null
            : dlopen(
                  darwin ? 'libSystem.B.dylib' : 'libc.so.6',
                  darwin
                      ? {
                            // int setxattr(path, name, value, size, u_int32_t position, int options)
                            setxattr: {
                                args: [
                                    FFIType.ptr,
                                    FFIType.ptr,
                                    FFIType.ptr,
                                    FFIType.u64,
                                    FFIType.u32,
                                    FFIType.i32
                                ],
                                returns: FFIType.i32
                            },
                            // ssize_t getxattr(path, name, value, size, u_int32_t position, int options)
                            getxattr: {
                                args: [
                                    FFIType.ptr,
                                    FFIType.ptr,
                                    FFIType.ptr,
                                    FFIType.u64,
                                    FFIType.u32,
                                    FFIType.i32
                                ],
                                returns: FFIType.i64
                            },
                            // ssize_t listxattr(path, namebuf, size, int options)
                            listxattr: {
                                args: [FFIType.ptr, FFIType.ptr, FFIType.u64, FFIType.i32],
                                returns: FFIType.i64
                            },
                            // int removexattr(path, name, int options)
                            removexattr: {
                                args: [FFIType.ptr, FFIType.ptr, FFIType.i32],
                                returns: FFIType.i32
                            },
                            fgetxattr: {
                                args: [
                                    FFIType.i32,
                                    FFIType.ptr,
                                    FFIType.ptr,
                                    FFIType.u64,
                                    FFIType.u32,
                                    FFIType.i32
                                ],
                                returns: FFIType.i64
                            },
                            flistxattr: {
                                args: [FFIType.i32, FFIType.ptr, FFIType.u64, FFIType.i32],
                                returns: FFIType.i64
                            },
                            fsetxattr: {
                                args: [
                                    FFIType.i32,
                                    FFIType.ptr,
                                    FFIType.ptr,
                                    FFIType.u64,
                                    FFIType.u32,
                                    FFIType.i32
                                ],
                                returns: FFIType.i32
                            },
                            fremovexattr: {
                                args: [FFIType.i32, FFIType.ptr, FFIType.i32],
                                returns: FFIType.i32
                            },
                            __error: { args: [], returns: FFIType.ptr },
                            strerror: { args: [FFIType.i32], returns: FFIType.cstring }
                        }
                      : {
                            // int setxattr(path, name, value, size, int flags)
                            lsetxattr: {
                                args: [
                                    FFIType.ptr,
                                    FFIType.ptr,
                                    FFIType.ptr,
                                    FFIType.u64,
                                    FFIType.i32
                                ],
                                returns: FFIType.i32
                            },
                            // ssize_t getxattr(path, name, value, size)
                            lgetxattr: {
                                args: [FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.u64],
                                returns: FFIType.i64
                            },
                            // ssize_t listxattr(path, list, size)
                            llistxattr: {
                                args: [FFIType.ptr, FFIType.ptr, FFIType.u64],
                                returns: FFIType.i64
                            },
                            // int removexattr(path, name)
                            lremovexattr: {
                                args: [FFIType.ptr, FFIType.ptr],
                                returns: FFIType.i32
                            },
                            fgetxattr: {
                                args: [FFIType.i32, FFIType.ptr, FFIType.ptr, FFIType.u64],
                                returns: FFIType.i64
                            },
                            flistxattr: {
                                args: [FFIType.i32, FFIType.ptr, FFIType.u64],
                                returns: FFIType.i64
                            },
                            fsetxattr: {
                                args: [
                                    FFIType.i32,
                                    FFIType.ptr,
                                    FFIType.ptr,
                                    FFIType.u64,
                                    FFIType.i32
                                ],
                                returns: FFIType.i32
                            },
                            fremovexattr: {
                                args: [FFIType.i32, FFIType.ptr],
                                returns: FFIType.i32
                            },
                            __errno_location: { args: [], returns: FFIType.ptr },
                            strerror: { args: [FFIType.i32], returns: FFIType.cstring }
                        }
              ).symbols
    )
)

const nativeSetXattr = darwin ? libc?.setxattr : libc?.lsetxattr
const nativeGetXattr = darwin ? libc?.getxattr : libc?.lgetxattr
const nativeListXattr = darwin ? libc?.listxattr : libc?.llistxattr
const nativeRemoveXattr = darwin ? libc?.removexattr : libc?.lremovexattr
const nativeGetXattrFd = libc?.fgetxattr
const nativeListXattrFd = libc?.flistxattr
const nativeSetXattrFd = libc?.fsetxattr
const nativeRemoveXattrFd = libc?.fremovexattr
const XATTR_NOFOLLOW = 0x0001

const errnoLocation = /** @type {() => import('bun:ffi').Pointer} */ (
    windows ? () => null : darwin ? libc?.__error : libc?.__errno_location
)

const ENOATTR = darwin ? 93 : 61 // ENOATTR (darwin) / ENODATA (linux)
const ERANGE = 34

const encoder = new TextEncoder()

/**
 * @param {string} text
 * @returns {Uint8Array} NUL-terminated UTF-8 bytes for C string arguments
 */
function cstr(text) {
    return encoder.encode(`${text}\0`)
}

/**
 * @returns {number}
 */
function errno() {
    return read.i32(errnoLocation(), 0)
}

/** @param {string} target */
function windowsStream(target) {
    return `${target}:fylo.xattrs`
}

/** @param {string} target */
function flushWindowsStream(target) {
    const descriptor = openSync(target, 'r+')
    try {
        fsyncSync(descriptor)
    } finally {
        closeSync(descriptor)
    }
}

/** @param {string} target @param {string} [call] @returns {void} */
function assertXattrTarget(target, call = 'xattr') {
    let metadata
    try {
        metadata = lstatSync(target)
    } catch (cause) {
        const source = /** @type {NodeJS.ErrnoException} */ (cause)
        const error = /** @type {NodeJS.ErrnoException} */ (
            new Error(`${call}(${target}) failed: ${source.message}`)
        )
        error.code = source.code
        error.errno = source.errno
        throw error
    }
    if (metadata.isSymbolicLink() || !metadata.isFile()) {
        throw new Error(`Extended attributes require a regular, non-link file: ${target}`)
    }
}

/** @param {string} target @param {string} call */
function assertWindowsTarget(target, call) {
    try {
        assertXattrTarget(target, call)
    } catch (cause) {
        const source = /** @type {NodeJS.ErrnoException} */ (cause)
        const error = /** @type {NodeJS.ErrnoException} */ (
            new Error(`${call}(${target}) failed: ${source.message}`)
        )
        error.code = source.code
        error.errno = source.errno
        throw error
    }
}

/**
 * Synchronous Windows ADS manifest store. All readers and read-modify-writers
 * share a per-file sentinel ADS, preventing lost updates across processes.
 * A recovery ADS is written before the manifest; if a process dies during the
 * copy, the next reader repairs the manifest from that complete recovery copy.
 */
export class WindowsAdsManifestStore {
    /** @param {{ lockTimeoutMs?: number, staleLockMs?: number }=} options */
    constructor(options = {}) {
        this.lockTimeoutMs = options.lockTimeoutMs ?? 5_000
        this.staleLockMs = options.staleLockMs ?? 30_000
    }

    /** @param {string} target @returns {string} */
    lockPath(target) {
        return `${target}:fylo.xattrs.lock`
    }

    /** @param {string} target @returns {string} */
    recoveryPath(target) {
        return `${target}:fylo.xattrs.next`
    }

    /** @param {string} target @param {string} call @param {() => any} operation */
    withLock(target, call, operation) {
        assertWindowsTarget(target, call)
        const lockPath = this.lockPath(target)
        const deadline = Date.now() + this.lockTimeoutMs
        if (windows) {
            let release = null
            while (!release) {
                release = windowsTryAcquireProcessFileLock(lockPath)
                if (release) break
                if (Date.now() >= deadline) {
                    throw new Error(
                        `${call}(${target}) timed out waiting for the ADS metadata lock`
                    )
                }
                Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 5)
            }
            try {
                return operation()
            } finally {
                release()
            }
        }
        const owner = String(Bun.randomUUIDv7())
        while (true) {
            try {
                const descriptor = openSync(lockPath, 'wx')
                try {
                    writeFileSync(
                        descriptor,
                        JSON.stringify({ owner, pid: process.pid, ts: Date.now() })
                    )
                } finally {
                    closeSync(descriptor)
                }
                break
            } catch (error) {
                if (/** @type {NodeJS.ErrnoException} */ (error).code !== 'EEXIST') throw error
                try {
                    const observed = readWindowsLock(lockPath)
                    const observedStat = statSync(lockPath)
                    const observedTimestamp = observed?.ts ?? observedStat.mtimeMs
                    if (Date.now() - observedTimestamp > this.staleLockMs) {
                        // Re-read immediately before takeover. A live owner may
                        // have replaced/refreshed the sentinel since the first
                        // observation; never unlink a different generation.
                        const current = readWindowsLock(lockPath)
                        const currentStat = statSync(lockPath)
                        const unchanged = observed
                            ? sameWindowsLock(observed, current)
                            : current === null &&
                              currentStat.mtimeMs === observedStat.mtimeMs &&
                              currentStat.size === observedStat.size
                        if (!unchanged) continue
                        unlinkSync(lockPath)
                        continue
                    }
                } catch (staleError) {
                    if (/** @type {NodeJS.ErrnoException} */ (staleError).code === 'ENOENT')
                        continue
                    throw staleError
                }
                if (Date.now() >= deadline) {
                    throw new Error(
                        `${call}(${target}) timed out waiting for the ADS metadata lock`
                    )
                }
                Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 5)
            }
        }
        try {
            return operation()
        } finally {
            // A stale-lock takeover can happen while a holder is suspended.
            // Only the generation we acquired may remove the sentinel.
            const current = readWindowsLock(lockPath)
            if (current?.owner === owner) {
                try {
                    unlinkSync(lockPath)
                } catch (error) {
                    if (/** @type {NodeJS.ErrnoException} */ (error).code !== 'ENOENT') throw error
                }
            }
        }
    }

    /** @param {string} target @param {string} call @returns {Record<string, string>} */
    read(target, call) {
        return this.withLock(target, call, () => this.readUnlocked(target))
    }

    /** @param {string} target @returns {Record<string, string>} */
    readUnlocked(target) {
        // `.next` is the write-ahead recovery copy. It must be considered
        // before a valid primary: a crash after writing `.next` but before the
        // copy leaves an older, perfectly valid primary that must not win.
        try {
            const recovery = this.recoveryPath(target)
            const recoveryPayload = readFileSync(recovery, 'utf8')
            const recovered = parseWindowsManifest(recoveryPayload)
            // Bun's Windows copyFile implementation does not reliably copy
            // between two named streams on the same base file. The validated
            // recovery payload is already in memory, so promote it directly.
            writeFileSync(windowsStream(target), recoveryPayload)
            flushWindowsStream(windowsStream(target))
            try {
                unlinkSync(recovery)
            } catch (error) {
                if (/** @type {NodeJS.ErrnoException} */ (error).code !== 'ENOENT') throw error
            }
            return recovered
        } catch (recoveryError) {
            try {
                return parseWindowsManifest(readFileSync(windowsStream(target), 'utf8'))
            } catch (primaryError) {
                if (
                    /** @type {NodeJS.ErrnoException} */ (primaryError).code === 'ENOENT' &&
                    /** @type {NodeJS.ErrnoException} */ (recoveryError).code === 'ENOENT'
                )
                    return {}
                // A corrupt/incomplete recovery copy does not invalidate an
                // intact primary, but if neither is usable report the primary
                // failure as the authoritative manifest error.
                throw primaryError
            }
        }
    }

    /**
     * @param {string} target
     * @param {string} call
     * @param {(attributes: Record<string, string>) => void} mutate
     */
    update(target, call, mutate) {
        this.withLock(target, call, () => {
            const attributes = this.readUnlocked(target)
            mutate(attributes)
            const recovery = this.recoveryPath(target)
            const payload = JSON.stringify(attributes)
            writeFileSync(recovery, payload)
            flushWindowsStream(recovery)
            writeFileSync(windowsStream(target), payload)
            flushWindowsStream(windowsStream(target))
            try {
                unlinkSync(recovery)
            } catch (error) {
                if (/** @type {NodeJS.ErrnoException} */ (error).code !== 'ENOENT') throw error
            }
        })
    }
}

/** @param {string} lockPath @returns {{ owner: string, ts: number } | null} */
function readWindowsLock(lockPath) {
    try {
        const parsed = /** @type {{ owner?: unknown, ts?: unknown }} */ (
            JSON.parse(readFileSync(lockPath, 'utf8'))
        )
        if (typeof parsed.owner !== 'string' || typeof parsed.ts !== 'number') return null
        return { owner: parsed.owner, ts: parsed.ts }
    } catch {
        return null
    }
}

/**
 * @param {{ owner: string, ts: number } | null} left
 * @param {{ owner: string, ts: number } | null} right
 */
function sameWindowsLock(left, right) {
    return left !== null && right !== null && left.owner === right.owner && left.ts === right.ts
}

/** @param {string} text @returns {Record<string, string>} */
function parseWindowsManifest(text) {
    const parsed = JSON.parse(text)
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
        throw new Error('Invalid Windows ADS metadata manifest')
    }
    for (const value of Object.values(parsed)) {
        if (typeof value !== 'string') throw new Error('Invalid Windows ADS metadata manifest')
    }
    return /** @type {Record<string, string>} */ (parsed)
}

const windowsStore = new WindowsAdsManifestStore()

const WINDOWS_DESCRIPTOR_MANIFEST_LIMIT = 16 * 1024 * 1024

/** @param {any} descriptor */
function readWindowsDescriptorManifestUnlocked(descriptor) {
    const recovery = windowsReadNamedStream(
        descriptor,
        'fylo.xattrs.next',
        WINDOWS_DESCRIPTOR_MANIFEST_LIMIT
    )
    if (recovery !== null) {
        try {
            const parsed = parseWindowsManifest(recovery.toString('utf8'))
            windowsWriteNamedStream(descriptor, 'fylo.xattrs', recovery)
            windowsDeleteNamedStream(descriptor, 'fylo.xattrs.next')
            return parsed
        } catch {
            // An incomplete recovery stream does not invalidate an intact
            // primary manifest; use the primary below.
        }
    }
    const primary = windowsReadNamedStream(
        descriptor,
        'fylo.xattrs',
        WINDOWS_DESCRIPTOR_MANIFEST_LIMIT
    )
    return primary === null ? {} : parseWindowsManifest(primary.toString('utf8'))
}

/** @param {any} descriptor */
function readWindowsDescriptorManifest(descriptor) {
    return windowsWithDescriptorLock(descriptor, () =>
        readWindowsDescriptorManifestUnlocked(descriptor)
    )
}

/** @param {any} descriptor @param {(attributes: Record<string, string>) => void} mutate */
function updateWindowsDescriptorManifest(descriptor, mutate) {
    windowsWithDescriptorLock(descriptor, () => {
        const attributes = readWindowsDescriptorManifestUnlocked(descriptor)
        mutate(attributes)
        const payload = Buffer.from(JSON.stringify(attributes))
        windowsWriteNamedStream(descriptor, 'fylo.xattrs.next', payload)
        windowsWriteNamedStream(descriptor, 'fylo.xattrs', payload)
        windowsDeleteNamedStream(descriptor, 'fylo.xattrs.next')
    })
}

/**
 * @param {string} call
 * @param {string} target
 * @returns {never}
 */
function throwErrno(call, target) {
    const code = errno()
    const name = Object.entries(os.constants.errno).find(([, value]) => value === code)?.[0]
    const error = /** @type {NodeJS.ErrnoException} */ (
        new Error(
            `${call}(${target}) failed: ${libc?.strerror(code) ?? 'unknown error'} (${name ?? `errno ${code}`})`
        )
    )
    error.code = name
    error.errno = code
    throw error
}

/**
 * Read one extended attribute.
 * @param {string} target
 * @param {string} name
 * @returns {Uint8Array | null} attribute bytes, or null when the attribute is absent
 */
export function getXattr(target, name) {
    if (windows) {
        const encoded = windowsStore.read(target, 'getxattr')[name]
        return typeof encoded === 'string' ? Uint8Array.from(Buffer.from(encoded, 'base64')) : null
    }
    assertXattrTarget(target, 'getxattr')
    if (!libc) throw new Error('Native xattrs are unavailable on this platform')
    const targetBytes = cstr(target)
    const nameBytes = cstr(name)
    // Size can change between the probe and the read, so retry on ERANGE.
    while (true) {
        const size = darwin
            ? nativeGetXattr?.(ptr(targetBytes), ptr(nameBytes), null, 0, 0, XATTR_NOFOLLOW)
            : nativeGetXattr?.(ptr(targetBytes), ptr(nameBytes), null, 0)
        if (size === undefined) throw new Error('Native xattrs are unavailable on this platform')
        if (size < 0) {
            if (errno() === ENOATTR) return null
            throwErrno('getxattr', target)
        }
        const value = new Uint8Array(Number(size))
        const buffer = value.length > 0 ? ptr(value) : null
        const written = darwin
            ? nativeGetXattr?.(
                  ptr(targetBytes),
                  ptr(nameBytes),
                  buffer,
                  value.length,
                  0,
                  XATTR_NOFOLLOW
              )
            : nativeGetXattr?.(ptr(targetBytes), ptr(nameBytes), buffer, value.length)
        if (written === undefined) throw new Error('Native xattrs are unavailable on this platform')
        if (written >= 0) return value.subarray(0, Number(written))
        const code = errno()
        if (code === ENOATTR) return null
        if (code !== ERANGE) throwErrno('getxattr', target)
    }
}

/**
 * Write one extended attribute, creating or replacing it.
 * @param {string} target
 * @param {string} name
 * @param {string | Uint8Array} value
 * @returns {void}
 */
export function setXattr(target, name, value) {
    const valueBytes = typeof value === 'string' ? encoder.encode(value) : value
    if (windows) {
        windowsStore.update(target, 'setxattr', (attributes) => {
            attributes[name] = Buffer.from(valueBytes).toString('base64')
        })
        return
    }
    assertXattrTarget(target, 'setxattr')
    if (!libc) throw new Error('Native xattrs are unavailable on this platform')
    const buffer = valueBytes.length > 0 ? ptr(valueBytes) : null
    const result = darwin
        ? nativeSetXattr?.(
              ptr(cstr(target)),
              ptr(cstr(name)),
              buffer,
              valueBytes.length,
              0,
              XATTR_NOFOLLOW
          )
        : nativeSetXattr?.(ptr(cstr(target)), ptr(cstr(name)), buffer, valueBytes.length, 0)
    if (result === undefined) throw new Error('Native xattrs are unavailable on this platform')
    if (result !== 0) throwErrno('setxattr', target)
}

/**
 * List extended attribute names.
 * @param {string} target
 * @returns {string[]}
 */
export function listXattr(target) {
    if (windows) return Object.keys(windowsStore.read(target, 'listxattr'))
    assertXattrTarget(target, 'listxattr')
    if (!libc) throw new Error('Native xattrs are unavailable on this platform')
    const targetBytes = cstr(target)
    while (true) {
        const size = darwin
            ? nativeListXattr?.(ptr(targetBytes), null, 0, XATTR_NOFOLLOW)
            : nativeListXattr?.(ptr(targetBytes), null, 0)
        if (size === undefined) throw new Error('Native xattrs are unavailable on this platform')
        if (size < 0) throwErrno('listxattr', target)
        if (size === 0n || size === 0) return []
        const names = new Uint8Array(Number(size))
        const written = darwin
            ? nativeListXattr?.(ptr(targetBytes), ptr(names), names.length, XATTR_NOFOLLOW)
            : nativeListXattr?.(ptr(targetBytes), ptr(names), names.length)
        if (written === undefined) throw new Error('Native xattrs are unavailable on this platform')
        if (written >= 0) {
            const joined = new TextDecoder().decode(names.subarray(0, Number(written)))
            return joined.split('\0').filter((name) => name.length > 0)
        }
        if (errno() !== ERANGE) throwErrno('listxattr', target)
    }
}

/**
 * Read one extended attribute from an already-open file descriptor. This keeps
 * metadata bound to the same inode as a race-safe file read.
 * @param {number} fd
 * @param {string} name
 * @returns {Uint8Array | null}
 */
export function getXattrFd(fd, name) {
    if (windows) {
        const encoded = readWindowsDescriptorManifest(fd)[name]
        return typeof encoded === 'string' ? Uint8Array.from(Buffer.from(encoded, 'base64')) : null
    }
    if (!nativeGetXattrFd) throw new Error('Native descriptor xattrs are unavailable')
    const nameBytes = cstr(name)
    while (true) {
        const size = darwin
            ? nativeGetXattrFd(fd, ptr(nameBytes), null, 0, 0, 0)
            : nativeGetXattrFd(fd, ptr(nameBytes), null, 0)
        if (size < 0) {
            if (errno() === ENOATTR) return null
            throwErrno('fgetxattr', `fd ${fd}`)
        }
        const value = new Uint8Array(Number(size))
        const buffer = value.length ? ptr(value) : null
        const written = darwin
            ? nativeGetXattrFd(fd, ptr(nameBytes), buffer, value.length, 0, 0)
            : nativeGetXattrFd(fd, ptr(nameBytes), buffer, value.length)
        if (written >= 0) return value.subarray(0, Number(written))
        const code = errno()
        if (code === ENOATTR) return null
        if (code !== ERANGE) throwErrno('fgetxattr', `fd ${fd}`)
    }
}

/** @param {number} fd @returns {string[]} */
export function listXattrFd(fd) {
    if (windows) return Object.keys(readWindowsDescriptorManifest(fd))
    if (!nativeListXattrFd) throw new Error('Native descriptor xattrs are unavailable')
    while (true) {
        const size = darwin ? nativeListXattrFd(fd, null, 0, 0) : nativeListXattrFd(fd, null, 0)
        if (size < 0) throwErrno('flistxattr', `fd ${fd}`)
        if (size === 0n || size === 0) return []
        const names = new Uint8Array(Number(size))
        const written = darwin
            ? nativeListXattrFd(fd, ptr(names), names.length, 0)
            : nativeListXattrFd(fd, ptr(names), names.length)
        if (written >= 0) {
            return new TextDecoder()
                .decode(names.subarray(0, Number(written)))
                .split('\0')
                .filter(Boolean)
        }
        if (errno() !== ERANGE) throwErrno('flistxattr', `fd ${fd}`)
    }
}

/** @param {number} fd @param {string} name @param {Uint8Array} value */
export function setXattrFd(fd, name, value) {
    if (windows) {
        updateWindowsDescriptorManifest(fd, (attributes) => {
            attributes[name] = Buffer.from(value).toString('base64')
        })
        return
    }
    if (!nativeSetXattrFd) throw new Error('Native descriptor xattrs are unavailable')
    const buffer = value.length ? ptr(value) : null
    const result = darwin
        ? nativeSetXattrFd(fd, ptr(cstr(name)), buffer, value.length, 0, 0)
        : nativeSetXattrFd(fd, ptr(cstr(name)), buffer, value.length, 0)
    if (result !== 0) throwErrno('fsetxattr', `fd ${fd}`)
}

/** @param {number} fd @param {string} name */
export function removeXattrFd(fd, name) {
    if (windows) {
        updateWindowsDescriptorManifest(fd, (attributes) => {
            delete attributes[name]
        })
        return
    }
    if (!nativeRemoveXattrFd) throw new Error('Native descriptor xattrs are unavailable')
    const result = darwin
        ? nativeRemoveXattrFd(fd, ptr(cstr(name)), 0)
        : nativeRemoveXattrFd(fd, ptr(cstr(name)))
    if (result !== 0 && errno() !== ENOATTR) throwErrno('fremovexattr', `fd ${fd}`)
}

/**
 * Remove one extended attribute. Removing an absent attribute is a no-op.
 * @param {string} target
 * @param {string} name
 * @returns {void}
 */
export function removeXattr(target, name) {
    if (windows) {
        windowsStore.update(target, 'removexattr', (attributes) => {
            delete attributes[name]
        })
        return
    }
    assertXattrTarget(target, 'removexattr')
    if (!libc) throw new Error('Native xattrs are unavailable on this platform')
    const result = darwin
        ? nativeRemoveXattr?.(ptr(cstr(target)), ptr(cstr(name)), XATTR_NOFOLLOW)
        : nativeRemoveXattr?.(ptr(cstr(target)), ptr(cstr(name)))
    if (result === undefined) throw new Error('Native xattrs are unavailable on this platform')
    if (result !== 0 && errno() !== ENOATTR) throwErrno('removexattr', target)
}
