import { describe, expect, test } from 'bun:test'
import { createHostImports } from '../../src/browser/host-vfs.mjs'
import {
    createAccessHandleBackend,
    createFileBackend,
    createRangeBackend
} from '../../src/browser/single-file-vfs.mjs'
import { hasFxdbMagic, isWorkingCopyName, workingCopyName } from '../../src/browser/fxdb-client.mjs'

const PATH = '/database.fxdb'
/** FileReaderSync is Worker-only; Bun has Blob but no synchronous reader. */
function readerOver(bytes) {
    return {
        readAsArrayBuffer(blob) {
            return bytes.slice(blob.start, blob.end).buffer
        }
    }
}

function blobOf(bytes) {
    return {
        size: bytes.length,
        lastModified: 42,
        slice: (start, end) => ({ start, end })
    }
}

/** A synchronous stand-in for FileSystemSyncAccessHandle over a growable array. */
function accessHandle(initial) {
    let bytes = initial.slice()
    return {
        getSize: () => bytes.length,
        read(into, { at }) {
            const part = bytes.subarray(at, at + into.length)
            into.set(part)
            return part.length
        },
        write(from, { at }) {
            if (at + from.length > bytes.length) {
                const grown = new Uint8Array(at + from.length)
                grown.set(bytes)
                bytes = grown
            }
            bytes.set(from, at)
            return from.length
        },
        truncate(length) {
            const next = new Uint8Array(length)
            next.set(bytes.subarray(0, length))
            bytes = next
        },
        flush() {},
        bytes: () => bytes
    }
}

/** A minimal XMLHttpRequest that answers byte ranges of `bytes`. */
function rangeServer(bytes, { etag = '"v1"', ranges = true } = {}) {
    const state = { etag, requests: [] }
    class Request {
        open(method, url, async) {
            this.headers = {}
            expect(async).toBe(false)
        }
        setRequestHeader(name, value) {
            this.headers[name] = value
        }
        send() {
            state.requests.push(this.headers)
            const ifRange = this.headers['If-Range']
            const match = /^bytes=(\d+)-(\d+)$/.exec(this.headers.Range)
            if (!ranges || (ifRange && ifRange !== state.etag)) {
                this.status = 200
                this.response = bytes.buffer
                this.responseHeaders = { ETag: state.etag }
                return
            }
            const first = Number(match[1])
            if (first >= bytes.length) {
                this.status = 416
                this.responseHeaders = {}
                return
            }
            const last = Math.min(Number(match[2]), bytes.length - 1)
            this.status = 206
            this.response = bytes.slice(first, last + 1).buffer
            this.responseHeaders = {
                'Content-Range': `bytes ${first}-${last}/${bytes.length}`,
                ETag: state.etag
            }
        }
        getResponseHeader(name) {
            return this.responseHeaders[name] ?? null
        }
    }
    return { Request, state }
}

function sample(length) {
    return Uint8Array.from({ length }, (_, index) => (index * 7 + 3) & 0xff)
}

describe('single-file host backends', () => {
    test('expose exactly one file and its directory', () => {
        const bytes = sample(100)
        const backend = createFileBackend(blobOf(bytes), { reader: readerOver(bytes) })
        expect(backend.stat(PATH)).toEqual({ kind: 1, len: 100, modifiedMs: 42 })
        expect(backend.stat('/')).toMatchObject({ kind: 2 })
        expect(backend.stat('/other')).toEqual({ kind: 0 })
        expect(backend.readDir('/')).toEqual(['database.fxdb'])
        expect(() => backend.open('/other', 1)).toThrow(expect.objectContaining({ code: 'ENOENT' }))
        expect(() => backend.open('/../etc/passwd', 1)).toThrow(
            expect.objectContaining({ code: 'EACCES' })
        )
        expect(() => backend.mkdir('/x', true)).toThrow(expect.objectContaining({ code: 'EROFS' }))
    })

    test('a picked file reads at any offset and refuses writes', () => {
        const bytes = sample(1000)
        const backend = createFileBackend(blobOf(bytes), { reader: readerOver(bytes) })
        const handle = backend.open(PATH, 1)
        const into = new Uint8Array(64)
        expect(backend.readAt(handle, 990, into)).toBe(10)
        expect([...into.subarray(0, 10)]).toEqual([...bytes.subarray(990)])
        expect(backend.readAt(handle, 2000, into)).toBe(0)
        expect(() => backend.open(PATH, 2)).toThrow(expect.objectContaining({ code: 'EROFS' }))
        expect(() => backend.writeAt(handle, 0, into)).toThrow(
            expect.objectContaining({ code: 'EROFS' })
        )
        expect(() => backend.truncate(handle, 0)).toThrow(
            expect.objectContaining({ code: 'EROFS' })
        )
    })

    test('a changed picked file is ESTALE, not torn', () => {
        const backend = createFileBackend(blobOf(sample(10)), {
            reader: {
                readAsArrayBuffer() {
                    throw Object.assign(new Error('gone'), { name: 'NotReadableError' })
                }
            }
        })
        const handle = backend.open(PATH, 1)
        expect(() => backend.readAt(handle, 0, new Uint8Array(4))).toThrow(
            expect.objectContaining({ code: 'ESTALE' })
        )
    })

    test('an OPFS working copy writes, grows and shrinks through the host table', () => {
        const handle = accessHandle(sample(50))
        const backend = createAccessHandleBackend(handle)
        const memory = new WebAssembly.Memory({ initial: 1 })
        const table = createHostImports({ memory: () => memory, backend }).fylo_host
        const view = new DataView(memory.buffer)
        const path = new TextEncoder().encode(PATH)
        new Uint8Array(memory.buffer).set(path, 64)
        expect(table.open(64, path.length, 1 | 2, 0)).toBe(0)
        const file = view.getBigUint64(0, true)
        new Uint8Array(memory.buffer).set([9, 9, 9], 1024)
        expect(table.write_at(file, 48n, 1024, 3, 8)).toBe(0)
        expect(view.getUint32(8, true)).toBe(3)
        expect(handle.getSize()).toBe(51)
        expect(table.truncate(file, 20n)).toBe(0)
        expect(handle.getSize()).toBe(20)
        expect(table.flush(file)).toBe(0)
        expect(table.stat(64, path.length, 16)).toBe(0)
        expect(Number(view.getBigUint64(24, true))).toBe(20)
        // A path outside the one file is refused with an errno, never a throw.
        expect(table.mkdir(64, 2, 1)).toBe(-13)
    })

    test('a hosted file reads by range and notices a replaced file', () => {
        const bytes = sample(300)
        const { Request, state } = rangeServer(bytes)
        const backend = createRangeBackend('https://host.invalid/db.fxdb', {
            XMLHttpRequest: Request
        })
        expect(backend.stat(PATH)).toMatchObject({ kind: 1, len: 300 })
        const handle = backend.open(PATH, 1)
        const into = new Uint8Array(40)
        expect(backend.readAt(handle, 280, into)).toBe(20)
        expect([...into.subarray(0, 20)]).toEqual([...bytes.subarray(280)])
        expect(state.requests.at(-1)).toEqual({ Range: 'bytes=280-299', 'If-Range': '"v1"' })
        state.etag = '"v2"'
        expect(() => backend.readAt(handle, 0, into)).toThrow(
            expect.objectContaining({ code: 'ESTALE' })
        )
    })

    test('a server that ignores ranges is refused rather than downloaded per read', () => {
        const { Request } = rangeServer(sample(10), { ranges: false })
        expect(() =>
            createRangeBackend('https://host.invalid/db.fxdb', { XMLHttpRequest: Request })
        ).toThrow(/byte-range/)
    })
})

describe('working copy names', () => {
    test('are plain .fxdb file names derived from the picked file', () => {
        expect(workingCopyName('orders.fxdb', 0)).toBe('orders-0.fxdb')
        expect(workingCopyName('../../x/y.fxdb', 35)).toBe('x-y-z.fxdb')
        expect(workingCopyName('', 1)).toBe('database-1.fxdb')
        for (const name of ['orders.fxdb', 'a (copy)-1.fxdb']) {
            expect(isWorkingCopyName(name)).toBe(true)
        }
        for (const name of ['.hidden.fxdb', 'a/b.fxdb', 'a..fxdb', 'orders.db', '']) {
            expect(isWorkingCopyName(name)).toBe(false)
        }
    })

    test('the magic check reads the PNG-style header', () => {
        expect(hasFxdbMagic([0x89, 0x46, 0x58, 0x44, 0x42, 0x0d, 0x0a, 0x1a, 0])).toBe(true)
        expect(hasFxdbMagic([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])).toBe(false)
    })
})
