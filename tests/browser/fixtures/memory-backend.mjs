// A `HostBackend` held entirely in memory.
//
// The engine's stack use does not depend on where bytes live, so the stack
// spec runs the module over this instead of OPFS: that lets it run in every
// browser, including Linux WebKit, which has no OPFS at all.

import { KIND_DIRECTORY, KIND_FILE, KIND_MISSING } from '/src/browser/host-vfs.mjs'
import { OPEN_CREATE, OPEN_EXCLUSIVE, OPEN_TRUNCATE } from '/src/browser/host-vfs.mjs'

export function createMemoryBackend() {
    /** @type {Map<string, Uint8Array>} */
    const files = new Map()
    const directories = new Set(['/'])
    /** @type {Map<string, Uint8Array>} */
    const attributes = new Map()
    /** @type {Map<number, string | null>} */
    const handles = new Map()
    let next = 1

    const fail = (code, message) => {
        const error = new Error(message)
        error.code = code
        throw error
    }
    const parentOf = (path) => path.slice(0, path.lastIndexOf('/')) || '/'
    const fileOf = (handle) => {
        const path = handles.get(handle)
        if (typeof path !== 'string') fail('EBADF', `not an open file: ${handle}`)
        return path
    }
    const below = (path) => (key) => key.startsWith(`${path}/`)

    return {
        open(path, flags) {
            const handle = next++
            if (directories.has(path)) {
                handles.set(handle, null)
                return handle
            }
            if (flags & OPEN_EXCLUSIVE && files.has(path)) fail('EEXIST', path)
            if (!files.has(path)) {
                if (!(flags & (OPEN_CREATE | OPEN_EXCLUSIVE))) fail('ENOENT', path)
                if (!directories.has(parentOf(path))) fail('ENOENT', path)
                files.set(path, new Uint8Array())
            }
            if (flags & OPEN_TRUNCATE) files.set(path, new Uint8Array())
            handles.set(handle, path)
            return handle
        },
        close(handle) {
            handles.delete(handle)
        },
        readAt(handle, offset, into) {
            const file = files.get(fileOf(handle)) ?? new Uint8Array()
            const length = Math.max(0, Math.min(into.length, file.length - offset))
            into.set(file.subarray(offset, offset + length))
            return length
        },
        writeAt(handle, offset, from) {
            const path = fileOf(handle)
            let file = files.get(path) ?? new Uint8Array()
            if (offset + from.length > file.length) {
                const grown = new Uint8Array(offset + from.length)
                grown.set(file)
                file = grown
            }
            file.set(from, offset)
            files.set(path, file)
            return from.length
        },
        truncate(handle, length) {
            const path = fileOf(handle)
            const resized = new Uint8Array(length)
            const file = files.get(path) ?? new Uint8Array()
            resized.set(file.subarray(0, Math.min(length, file.length)))
            files.set(path, resized)
        },
        flush() {},
        stat(path) {
            if (directories.has(path)) return { kind: KIND_DIRECTORY, len: 0, modifiedMs: 0 }
            const file = files.get(path)
            if (!file) return { kind: KIND_MISSING, len: 0, modifiedMs: 0 }
            return { kind: KIND_FILE, len: file.length, modifiedMs: 0 }
        },
        mkdir(path, recursive) {
            if (directories.has(path)) {
                if (!recursive) fail('EEXIST', path)
                return
            }
            if (!directories.has(parentOf(path))) {
                if (!recursive) fail('ENOENT', path)
                this.mkdir(parentOf(path), true)
            }
            directories.add(path)
        },
        unlink(path) {
            if (!files.delete(path)) fail('ENOENT', path)
        },
        rmdir(path, recursive) {
            const inside = below(path)
            const children = [...files.keys(), ...directories].filter(inside)
            if (children.length > 0 && !recursive) fail('EIO', `not empty: ${path}`)
            for (const child of children) {
                files.delete(child)
                directories.delete(child)
            }
            if (!directories.delete(path)) fail('ENOENT', path)
        },
        rename(from, to) {
            if (files.has(from)) {
                files.set(to, files.get(from))
                files.delete(from)
                return
            }
            if (!directories.has(from)) fail('ENOENT', from)
            const inside = below(from)
            for (const key of [...files.keys()].filter(inside)) {
                files.set(to + key.slice(from.length), files.get(key))
                files.delete(key)
            }
            for (const key of [...directories].filter((key) => key === from || inside(key))) {
                directories.delete(key)
                directories.add(to + key.slice(from.length))
            }
        },
        readDir(path) {
            if (!directories.has(path)) fail('ENOENT', path)
            const prefix = path === '/' ? '/' : `${path}/`
            const names = new Set()
            for (const key of [...files.keys(), ...directories]) {
                if (key !== path && key.startsWith(prefix)) {
                    names.add(key.slice(prefix.length).split('/')[0])
                }
            }
            return [...names]
        },
        readAttrs(path) {
            return attributes.get(path) ?? new Uint8Array()
        },
        writeAttrs(path, manifest) {
            if (manifest.length === 0) attributes.delete(path)
            else attributes.set(path, manifest.slice())
        },
        random(into) {
            crypto.getRandomValues(into)
        },
        nowUnixMs() {
            return Date.now()
        },
        log(message) {
            console.error(message)
        }
    }
}
