import { dlopen, FFIType, ptr, read } from 'bun:ffi'
import { closeSync, constants, openSync, realpathSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const library = process.platform === 'darwin' ? 'libSystem.B.dylib' : 'libc.so.6'
const symbols =
    process.platform === 'win32'
        ? null
        : dlopen(library, {
              openat: {
                  args: [FFIType.i32, FFIType.ptr, FFIType.i32],
                  returns: FFIType.i32
              },
              ...(process.platform === 'darwin'
                  ? { __error: { args: [], returns: FFIType.ptr } }
                  : { __errno_location: { args: [], returns: FFIType.ptr } })
          }).symbols
const openat = symbols?.openat
const encoder = new TextEncoder()

/** @param {string} value */
function cstr(value) {
    return encoder.encode(`${value}\0`)
}

/** @returns {number} */
function errno() {
    const location =
        process.platform === 'darwin'
            ? /** @type {any} */ (symbols).__error()
            : /** @type {any} */ (symbols).__errno_location()
    return read.i32(location, 0)
}

/** @param {string} target @param {boolean} allowUnsafe */
function openFailure(target, allowUnsafe) {
    const code = errno()
    const name = Object.entries(os.constants.errno).find(([, value]) => value === code)?.[0]
    if (allowUnsafe && (name === 'ENOENT' || name === 'ELOOP' || name === 'ENOTDIR')) return null
    const error = /** @type {NodeJS.ErrnoException} */ (
        new Error(`Secure open failed for ${target}: ${name ?? `errno ${code}`}`)
    )
    error.code = name
    error.errno = code
    throw error
}

/**
 * Open a regular-file candidate by walking from an already-open root directory.
 * Every component uses O_NOFOLLOW, so renaming or replacing any parent cannot
 * redirect the final descriptor outside the root inode.
 *
 * @param {string} target
 * @returns {number} caller-owned descriptor for the pinned directory
 */
export function openDirectoryNoFollow(target) {
    if (!openat) throw new Error('Secure descriptor traversal is unavailable on this platform')
    const resolved = realpathSync(target)
    const parsed = path.parse(resolved)
    const parts = resolved.slice(parsed.root.length).split(path.sep).filter(Boolean)
    let current = openSync(parsed.root, constants.O_RDONLY | constants.O_DIRECTORY)
    try {
        for (const part of parts) {
            const next = openat(
                current,
                ptr(cstr(part)),
                constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_DIRECTORY
            )
            if (next < 0) openFailure(target, false)
            closeSync(current)
            current = next
        }
        const result = current
        current = -1
        return result
    } finally {
        if (current >= 0) closeSync(current)
    }
}

/**
 * @param {number} rootFd pinned descriptor for the FYLO root directory
 * @param {string} relative
 * @returns {number | null}
 */
export function openFileAtRoot(rootFd, relative) {
    if (!openat) throw new Error('Secure descriptor traversal is unavailable on this platform')
    const parts = relative.split(path.sep)
    if (!relative || parts.some((part) => !part || part === '.' || part === '..')) return null

    let current = rootFd
    let ownsCurrent = false
    try {
        for (let index = 0; index < parts.length; index += 1) {
            const directory = index < parts.length - 1
            const flags =
                constants.O_RDONLY | constants.O_NOFOLLOW | (directory ? constants.O_DIRECTORY : 0)
            const next = openat(current, ptr(cstr(parts[index])), flags)
            if (next < 0) return openFailure(relative, true)
            if (ownsCurrent) closeSync(current)
            current = next
            ownsCurrent = true
        }
        const result = current
        ownsCurrent = false
        return result
    } finally {
        if (ownsCurrent) closeSync(current)
    }
}
