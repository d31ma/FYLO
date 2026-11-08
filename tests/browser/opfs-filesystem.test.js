import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { OpfsFilesystem } from '../../src/browser/opfs-filesystem.js'
import { createBrowserClient } from '../../src/browser/client.js'

class DetachedDirectoryError extends Error {
    constructor() {
        super('The operation failed for an unknown transient reason (e.g. out of memory).')
        this.name = 'UnknownError'
    }
}

class MissingEntryError extends Error {
    constructor() {
        super('The object can not be found here.')
        this.name = 'NotFoundError'
    }
}

class WrongKindError extends Error {
    constructor() {
        super('File type is incompatible with handle type')
        this.name = 'TypeMismatchError'
    }
}

class FakeFileHandle {
    kind = 'file'
    bytes = new Uint8Array(0)
    lastModified = Date.now()

    async getFile() {
        const bytes = this.bytes
        return {
            lastModified: this.lastModified,
            size: bytes.byteLength,
            arrayBuffer: async () => bytes.slice().buffer
        }
    }

    async createWritable() {
        const file = this
        const chunks = []
        return {
            async write(data) {
                chunks.push(new Uint8Array(data))
            },
            async close() {
                file.bytes = new Uint8Array(Buffer.concat(chunks))
                file.lastModified = Date.now()
            }
        }
    }
}

class FakeDirectoryHandle {
    kind = 'directory'
    detached = false
    /** @type {Map<string, FakeDirectoryHandle | FakeFileHandle>} */
    entries = new Map()

    requireAttached() {
        if (this.detached) throw new DetachedDirectoryError()
    }

    async getDirectoryHandle(name, options = {}) {
        this.requireAttached()
        const existing = this.entries.get(name)
        if (existing instanceof FakeDirectoryHandle) return existing
        if (existing instanceof FakeFileHandle) throw new WrongKindError()
        if (!options.create) throw new MissingEntryError()
        const created = new FakeDirectoryHandle()
        this.entries.set(name, created)
        return created
    }

    async getFileHandle(name, options = {}) {
        this.requireAttached()
        const existing = this.entries.get(name)
        if (existing instanceof FakeFileHandle) return existing
        if (existing instanceof FakeDirectoryHandle) throw new WrongKindError()
        if (!options.create) throw new MissingEntryError()
        const created = new FakeFileHandle()
        this.entries.set(name, created)
        return created
    }

    async removeEntry(name) {
        this.requireAttached()
        const existing = this.entries.get(name)
        if (!existing) throw new MissingEntryError()
        existing.detach?.()
        this.entries.delete(name)
    }

    detach() {
        this.detached = true
        for (const entry of this.entries.values()) entry.detach?.()
    }

    async *keys() {
        this.requireAttached()
        yield* this.entries.keys()
    }
}

const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator')

beforeEach(() => {
    const root = new FakeDirectoryHandle()
    Object.defineProperty(globalThis, 'navigator', {
        configurable: true,
        writable: true,
        value: { storage: { getDirectory: async () => root } }
    })
})

afterEach(() => {
    Object.defineProperty(globalThis, 'navigator', originalNavigator)
})

describe('OpfsFilesystem', () => {
    test('writes into a directory recreated after a recursive remove', async () => {
        const fs = new OpfsFilesystem({ namespace: 'fylo' })
        await fs.mkdir('/c/index', { recursive: true })
        await fs.writeText('/c/index/manifest.json', '{"a":1}')

        await fs.rmdir('/c/index', { recursive: true })
        await fs.mkdir('/c/index', { recursive: true })
        await fs.writeText('/c/index/manifest.json', '{"b":2}')

        expect(await fs.readText('/c/index/manifest.json')).toBe('{"b":2}')
    })

    test('forgets cached child directories of a removed directory', async () => {
        const fs = new OpfsFilesystem({ namespace: 'fylo' })
        await fs.writeText('/c/docs/A/one.json', '{}')

        await fs.rmdir('/c', { recursive: true })
        await fs.mkdir('/c/docs/A', { recursive: true })
        await fs.writeText('/c/docs/A/two.json', '{}')

        expect(await fs.list('/c/docs/A')).toEqual(['two.json'])
    })

    test('keeps a sibling directory whose name shares the removed prefix', async () => {
        const fs = new OpfsFilesystem({ namespace: 'fylo' })
        await fs.writeText('/c/index/a.json', '1')
        await fs.writeText('/c/index-old/b.json', '2')

        await fs.rmdir('/c/index', { recursive: true })

        expect(await fs.readText('/c/index-old/b.json')).toBe('2')
    })
})

describe('browser engine on OPFS', () => {
    test('rebuilds a collection index', async () => {
        const fylo = createBrowserClient({ storage: 'opfs', worker: false })
        await fylo.collection('users').create()
        const id = await fylo.collection('users').put({ name: 'Ada', role: 'admin' })

        const rebuilt = await fylo.collection('users').rebuild()
        const latest = await fylo.collection('users').latest(id)

        expect(rebuilt).toMatchObject({ collection: 'users', docsScanned: 1, indexedDocs: 1 })
        expect(latest).toEqual({ [id]: { name: 'Ada', role: 'admin' } })
    })

    test('recreates a dropped collection with the same name', async () => {
        const fylo = createBrowserClient({ storage: 'opfs', worker: false })
        await fylo.collection('users').create()
        await fylo.collection('users').put({ name: 'Ada' })

        await fylo.collection('users').drop()
        await fylo.collection('users').create()
        const id = await fylo.collection('users').put({ name: 'Grace' })

        expect(await fylo.collection('users').latest(id)).toEqual({ [id]: { name: 'Grace' } })
    })
})
