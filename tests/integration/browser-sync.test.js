import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import { createFyloHttpHandler } from '../../src/server/http.js'
import { createSyncedClient } from '../../src/browser/sync/index.js'
import { createTestRoot } from '../helpers/root.js'

const roots = []
const sleep = (/** @type {number} */ ms) => new Promise((r) => setTimeout(r, ms))

async function backend() {
    const root = await createTestRoot('fylo-sync-')
    roots.push(root)
    const handler = createFyloHttpHandler({ root, token: 't' })
    /** @type {typeof fetch} */
    const fetchImpl = (url, init) => handler(new Request(String(url), init))
    return { root, fetchImpl }
}

/** @param {any} fetchImpl @param {any} op */
async function exec(fetchImpl, op) {
    const res = await fetchImpl('http://fylo.test/v1/exec', {
        method: 'POST',
        headers: { authorization: 'Bearer t', 'content-type': 'application/json' },
        body: JSON.stringify(op)
    })
    return (await res.json()).result
}

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('local-first browser sync', () => {
    test('a local write pushes to the backend', async () => {
        const { fetchImpl } = await backend()
        const db = createSyncedClient({
            serverUrl: 'http://fylo.test',
            token: 't',
            storage: 'memory',
            fetch: fetchImpl
        })
        await db.ready()
        await db.sync.start()
        expect(db.sync.online).toBe(true)

        const id = await db.notes.put({ t: 'from-client' })
        await sleep(300)
        const onBackend = await exec(fetchImpl, { op: 'getLatest', collection: 'notes', id })
        expect(onBackend[id]).toEqual({ t: 'from-client' })
        db.sync.stop()
        await db.close()
    })

    test('a backend write pulls into the local store', async () => {
        const { fetchImpl } = await backend()
        const db = createSyncedClient({
            serverUrl: 'http://fylo.test',
            token: 't',
            storage: 'memory',
            fetch: fetchImpl
        })
        await db.ready()
        await db.sync.start()
        void db.notes // subscribe to the collection's changes feed

        await exec(fetchImpl, { op: 'createCollection', collection: 'notes' })
        const rid = await exec(fetchImpl, {
            op: 'putData',
            collection: 'notes',
            data: { t: 'from-backend' }
        })
        await sleep(600)
        const local = await db.notes.latest(rid)
        expect(local[rid]).toEqual({ t: 'from-backend' })
        db.sync.stop()
        await db.close()
    })

    test('concurrent edits resolve last-write-wins', async () => {
        const { fetchImpl } = await backend()
        const db = createSyncedClient({
            serverUrl: 'http://fylo.test',
            token: 't',
            storage: 'memory',
            fetch: fetchImpl
        })
        await db.ready()
        await db.sync.start()

        const id = await db.notes.put({ t: 'v1' })
        await sleep(200)
        // Backend edits the same doc, then the client edits it with a fresh (newer) timestamp.
        await exec(fetchImpl, {
            op: 'putData',
            collection: 'notes',
            data: { [id]: { t: 'backend-edit' } }
        })
        await sleep(50)
        await db.notes.put({ [id]: { t: 'client-edit' } })
        await sleep(300)
        const resolved = await exec(fetchImpl, { op: 'getLatest', collection: 'notes', id })
        expect(resolved[id]).toEqual({ t: 'client-edit' }) // client wrote last → wins
        db.sync.stop()
        await db.close()
    })

    test('offline writes stay local and queue for reconnect', async () => {
        const db = createSyncedClient({
            serverUrl: 'http://fylo.test',
            token: 't',
            storage: 'memory',
            fetch: () => Promise.reject(new Error('offline'))
        })
        await db.ready()
        await db.sync.start()
        expect(db.sync.online).toBe(false)

        const id = await db.notes.put({ t: 'offline' })
        const local = await db.notes.latest(id)
        expect(local[id]).toEqual({ t: 'offline' }) // works locally
        expect(db.sync.queue.length).toBeGreaterThan(0) // queued for later push
        db.sync.stop()
        await db.close()
    })

    test('repeated edits to one doc coalesce into a single pending push', async () => {
        const db = createSyncedClient({ storage: 'memory' }) // offline: writes stay queued
        await db.ready()
        await db.sync.start()

        const id = await db.notes.put({ v: 0 })
        for (let i = 1; i <= 5; i++) await db.notes.put({ [id]: { v: i } })

        const pending = db.sync.queue.filter((/** @type {any} */ c) => c.id === id)
        expect(pending.length).toBe(1) // 6 writes → 1 pending change
        expect(pending[0].doc).toEqual({ v: 5 }) // last write wins
        db.sync.stop()
        await db.close()
    })

    test('two clients converge through the backend', async () => {
        const { fetchImpl } = await backend()
        const mk = () =>
            createSyncedClient({
                serverUrl: 'http://fylo.test',
                token: 't',
                storage: 'memory',
                fetch: fetchImpl,
                batchMs: 0
            })
        const a = mk()
        const b = mk()
        await a.ready()
        await a.sync.start()
        void a.notes
        await b.ready()
        await b.sync.start()
        void b.notes

        const idA = await a.notes.put({ who: 'a' })
        await sleep(500)
        expect((await b.notes.latest(idA))[idA]).toEqual({ who: 'a' }) // b pulled a's write

        const idB = await b.notes.put({ who: 'b' })
        await sleep(500)
        expect((await a.notes.latest(idB))[idB]).toEqual({ who: 'b' }) // a pulled b's write

        a.sync.stop()
        b.sync.stop()
        await a.close()
        await b.close()
    })

    test('a large offline backlog flushes on reconnect', async () => {
        const { fetchImpl } = await backend()
        let up = false
        /** @type {typeof fetch} */
        const gated = (url, init) =>
            up ? fetchImpl(url, init) : Promise.reject(new Error('offline'))
        const db = createSyncedClient({
            serverUrl: 'http://fylo.test',
            token: 't',
            storage: 'memory',
            fetch: gated
        })
        await db.ready()
        await db.sync.start()
        expect(db.sync.online).toBe(false)

        const ids = []
        for (let i = 0; i < 25; i++) ids.push(await db.notes.put({ n: i }))
        expect(db.sync.queue.length).toBe(25) // all queued offline

        up = true
        await db.sync._checkConnectivity() // reconnect → flush the backlog
        await sleep(400)
        expect(db.sync.queue.length).toBe(0)
        for (const id of ids) {
            const onBackend = await exec(fetchImpl, { op: 'getLatest', collection: 'notes', id })
            expect(onBackend[id]).toBeDefined()
        }

        db.sync.stop()
        await db.close()
    })
})
