import { dlopen, FFIType, ptr, read } from 'bun:ffi'
import { closeSync, constants, openSync, realpathSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const encoder = new TextEncoder()

/** @param {NodeJS.Platform} platform @param {string} architecture */
export function libcCandidates(platform, architecture) {
    if (platform === 'darwin') return ['libSystem.B.dylib']
    if (platform !== 'linux') return []
    const muslArchitecture =
        {
            x64: 'x86_64',
            arm64: 'aarch64',
            arm: 'armhf'
        }[architecture] ?? architecture
    return [
        'libc.so.6',
        `libc.musl-${muslArchitecture}.so.1`,
        `/lib/ld-musl-${muslArchitecture}.so.1`
    ]
}

/** @param {NodeJS.Platform} platform */
function symbolDefinitions(platform) {
    return {
        openat: {
            args: [FFIType.i32, FFIType.ptr, FFIType.i32],
            returns: FFIType.i32
        },
        ...(platform === 'darwin'
            ? { __error: { args: [], returns: FFIType.ptr } }
            : { __errno_location: { args: [], returns: FFIType.ptr } })
    }
}

/**
 * Resolve the platform C library without making module import depend on a
 * particular libc distribution. Returning null keeps traversal fail-closed.
 *
 * @param {typeof dlopen} openLibrary
 * @param {string[]} candidates
 * @param {NodeJS.Platform} [platform]
 * @returns {Record<string, Function> | null}
 */
export function loadSecureOpenSymbols(
    openLibrary = dlopen,
    candidates = libcCandidates(process.platform, process.arch),
    platform = process.platform
) {
    for (const candidate of candidates) {
        try {
            return /** @type {any} */ (openLibrary(candidate, symbolDefinitions(platform))).symbols
        } catch {
            // Try the next ABI-compatible libc name. Absence is handled by callers.
        }
    }
    return null
}

/** @type {Record<string, Function> | null | undefined} */
let symbols

function secureOpenSymbols() {
    if (symbols === undefined) symbols = loadSecureOpenSymbols()
    return symbols
}

/** @param {string} value */
function cstr(value) {
    return encoder.encode(`${value}\0`)
}

/** @param {Record<string, Function>} symbols @returns {number} */
function errno(symbols) {
    const location =
        process.platform === 'darwin'
            ? /** @type {any} */ (symbols).__error()
            : /** @type {any} */ (symbols).__errno_location()
    return read.i32(location, 0)
}

/**
 * @param {string} target
 * @param {boolean} allowUnsafe
 * @param {Record<string, Function>} symbols
 */
function openFailure(target, allowUnsafe, symbols) {
    const code = errno(symbols)
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
    const symbols = secureOpenSymbols()
    const openat = symbols?.openat
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
            if (next < 0) openFailure(target, false, symbols)
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
    const symbols = secureOpenSymbols()
    const openat = symbols?.openat
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
            if (next < 0) return openFailure(relative, true, symbols)
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
