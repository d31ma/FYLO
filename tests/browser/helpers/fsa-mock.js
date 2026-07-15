import { mkdir, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'

// node:fs-backed stand-in for the W3C FileSystemDirectoryHandle surface the
// FSA adapter uses, so the real FsaFilesystem can be exercised against a real
// on-disk FYLO root without a browser. Error semantics mirror the spec:
// missing entry → NotFoundError, wrong kind → TypeMismatchError.
function notFound() {
    const error = new Error('A requested file or directory could not be found')
    error.name = 'NotFoundError'
    return error
}

function typeMismatch() {
    const error = new Error('The path supplied exists, but was not an entry of requested type')
    error.name = 'TypeMismatchError'
    return error
}

function mockFileHandle(file) {
    return {
        kind: 'file',
        name: path.basename(file),
        async getFile() {
            const [bytes, info] = await Promise.all([readFile(file), stat(file)])
            return {
                lastModified: info.mtimeMs,
                arrayBuffer: async () =>
                    bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength)
            }
        },
        async createWritable() {
            const chunks = []
            return {
                async write(data) {
                    chunks.push(Buffer.from(data))
                },
                async close() {
                    await writeFile(file, Buffer.concat(chunks))
                }
            }
        }
    }
}

/** @param {string} dir absolute directory to expose as a directory handle */
export function mockDirectoryHandle(dir) {
    return {
        kind: 'directory',
        name: path.basename(dir),
        async getDirectoryHandle(name, options = {}) {
            const target = path.join(dir, name)
            const info = await stat(target).catch(() => null)
            if (info && !info.isDirectory()) throw typeMismatch()
            if (!info) {
                if (!options.create) throw notFound()
                await mkdir(target, { recursive: true })
            }
            return mockDirectoryHandle(target)
        },
        async getFileHandle(name, options = {}) {
            const target = path.join(dir, name)
            const info = await stat(target).catch(() => null)
            if (info && !info.isFile()) throw typeMismatch()
            if (!info) {
                if (!options.create) throw notFound()
                await writeFile(target, new Uint8Array())
            }
            return mockFileHandle(target)
        },
        async *keys() {
            for (const name of await readdir(dir)) yield name
        },
        async removeEntry(name, options = {}) {
            try {
                await rm(path.join(dir, name), { recursive: options.recursive === true })
            } catch {
                throw notFound()
            }
        }
    }
}
