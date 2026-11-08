import { describe, expect, test } from 'bun:test'
import { createBrowserClient } from '../../src/browser/client.js'
import { createBrowserFylo } from '../../src/browser/fylo.js'
import { BrowserCore } from '../../src/browser/core/engine.js'
import { createMemoryFilesystem } from '../../src/browser/core/memory-filesystem.js'

const UNSUPPORTED = { code: 'EBROWSER_UNSUPPORTED', name: 'BrowserUnsupportedError' }

function memoryClient() {
    return createBrowserClient({ storage: 'memory', worker: false })
}

describe('browser engine fails closed on features it does not implement', () => {
    test.each([
        ['a file bucket', { kind: 'file' }],
        ['a schema', { schema: { name: '^.+$' } }],
        ['$encrypted fields', { $encrypted: ['ssn'] }],
        ['an encryption key', { encryption: { key: 'secret' } }],
        ['an encrypted flag', { encrypted: true }],
        ['a cipher', { cipher: { salt: 'salt' } }]
    ])('create with %s throws and creates nothing', async (_label, options) => {
        const fylo = memoryClient()

        const creating = fylo.collection('vault').create(options)

        await expect(creating).rejects.toMatchObject(UNSUPPORTED)
        expect(await fylo.collection('vault').inspect()).toMatchObject({ exists: false })
    })

    test('create without options and with the document kind still works', async () => {
        const fylo = memoryClient()

        await fylo.collection('plain').create()
        await fylo.collection('typed').create({ kind: 'document' })

        expect(await fylo.collection('plain').inspect()).toMatchObject({ exists: true })
        expect(await fylo.collection('typed').inspect()).toMatchObject({ exists: true })
    })

    test('the encryption error message says nothing was stored', async () => {
        const fylo = memoryClient()

        const creating = fylo.collection('vault').create({ $encrypted: ['ssn'] })

        await expect(creating).rejects.toThrow(
            'The FYLO browser engine does not support encryption ($encrypted); nothing was stored'
        )
    })

    test.each([
        ['a Blob', () => new Blob(['hello'])],
        ['a File', () => new File(['hello'], 'greeting.txt', { type: 'text/plain' })],
        ['a URL', () => new URL('https://example.com/a.txt')]
    ])('put of %s into a document collection throws and stores nothing', async (_label, value) => {
        const fylo = memoryClient()
        await fylo.collection('notes').create()

        const writing = fylo.collection('notes').put(value())

        await expect(writing).rejects.toMatchObject(UNSUPPORTED)
        expect(await fylo.collection('notes').inspect()).toMatchObject({ docsStored: 0 })
    })

    test('put of a Blob under an explicit id throws', async () => {
        const fylo = memoryClient()
        await fylo.collection('notes').create()

        const writing = fylo.collection('notes').put('4WNMX8R4TRG', new Blob(['hello']))

        await expect(Promise.resolve(writing)).rejects.toMatchObject(UNSUPPORTED)
    })

    test('a batch with one Blob throws before writing any document', async () => {
        const fylo = memoryClient()
        await fylo.collection('notes').create()

        const writing = fylo.collection('notes').batchPut([{ name: 'Ada' }, new Blob(['hello'])])

        await expect(writing).rejects.toMatchObject(UNSUPPORTED)
        expect(await fylo.collection('notes').inspect()).toMatchObject({ docsStored: 0 })
    })

    test('an encryption option on the client throws when it is created', () => {
        const creating = () => createBrowserClient({ storage: 'memory', encryption: { key: 'k' } })

        expect(creating).toThrow(expect.objectContaining({ code: 'EBROWSER_UNSUPPORTED' }))
    })

    test('the runtime facade rejects a file bucket with the same code', async () => {
        const fylo = createBrowserFylo({ storage: 'memory', worker: false })

        const creating = fylo['assets'].create({ kind: 'file' })

        await expect(creating).rejects.toMatchObject(UNSUPPORTED)
    })

    test('the protocol answers a file bucket request with the code', async () => {
        const fylo = createBrowserFylo({ storage: 'memory', worker: false })

        const response = await fylo.request({
            op: 'createCollection',
            collection: 'assets',
            options: { kind: 'file' }
        })

        expect(response.ok).toBe(false)
        expect(response).toMatchObject({ error: { code: 'EBROWSER_UNSUPPORTED' } })
    })

    test('the in-process core rejects a file bucket', async () => {
        const fylo = new BrowserCore({ fs: createMemoryFilesystem(), root: '/' })

        const creating = fylo['assets'].create({ kind: 'file' })

        await expect(creating).rejects.toMatchObject(UNSUPPORTED)
    })
})

describe('browser client executeSQL', () => {
    test('is a real method and not a collection', () => {
        const fylo = memoryClient()

        expect(typeof fylo.executeSQL).toBe('function')
        expect(fylo.executeSQL).toBe(fylo.executeSQL)
    })

    test('runs raw SQL against the local store', async () => {
        const fylo = memoryClient()

        await fylo.executeSQL('CREATE TABLE posts')
        await fylo.executeSQL("INSERT INTO posts (title) VALUES ('Hello')")
        const rows = await fylo.executeSQL("SELECT title FROM posts WHERE title = 'Hello'")

        expect(Object.values(/** @type {Record<string, unknown>} */ (rows))).toEqual([
            { title: 'Hello' }
        ])
    })

    test('runs raw SQL through the runtime', async () => {
        const fylo = createBrowserFylo({ storage: 'memory', worker: false })

        await fylo.executeSQL('CREATE TABLE posts')
        await fylo.executeSQL("INSERT INTO posts (title) VALUES ('Hello')")
        const rows = await fylo.executeSQL('SELECT title FROM posts')

        expect(Object.values(/** @type {Record<string, unknown>} */ (rows))).toEqual([
            { title: 'Hello' }
        ])
    })

    test('runs through the request protocol on the runtime', async () => {
        const fylo = createBrowserFylo({ storage: 'memory', worker: false })
        await fylo.executeSQL('CREATE TABLE posts')

        const response = await fylo.request({ op: 'executeSQL', sql: 'SELECT * FROM posts' })

        expect(response).toMatchObject({ ok: true, result: {} })
    })
})

describe('browser client errors', () => {
    test('a missing collection keeps the engine code', async () => {
        const fylo = memoryClient()

        const reading = fylo.collection('ghosts').latest('4WNMX8R4TRG')

        await expect(reading).rejects.toMatchObject({
            code: 'FYLO_COLLECTION_NOT_FOUND',
            message: 'Collection not found: ghosts'
        })
    })
})
