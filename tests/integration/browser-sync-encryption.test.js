import { afterAll, beforeAll, describe, expect, mock, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { createFyloHttpHandler } from '../../src/server/http.js'
import { createSyncedClient } from '../../src/browser/sync/index.js'
import { createTestRoot } from '../helpers/root.js'
import { CipherMock } from '../mocks/cipher.js'

mock.module('../../src/security/cipher', () => ({ Cipher: CipherMock }))

const roots = []
const COLLECTION = 'secrets'
const sleep = (/** @type {number} */ ms) => new Promise((r) => setTimeout(r, ms))

async function backend() {
    const root = await createTestRoot('fylo-sync-enc-')
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

/** @param {string} serverUrl @param {any} fetchImpl */
function client(serverUrl, fetchImpl) {
    return createSyncedClient({
        serverUrl,
        token: 't',
        storage: 'memory',
        fetch: fetchImpl,
        batchMs: 0
    })
}

beforeAll(async () => {
    await CipherMock.configure('test-secret')
    CipherMock.registerFields(COLLECTION, ['secret'])
})

afterAll(async () => {
    CipherMock.reset()
    mock.restore()
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('local-first sync with encrypted fields', () => {
    test('encrypted fields sync as plaintext but persist encrypted at rest', async () => {
        const { root, fetchImpl } = await backend()

        const a = client('http://fylo.test', fetchImpl)
        await a.ready()
        await a.sync.start()
        const id = await a.secrets.put({ name: 'ada', secret: 'hunter2' })
        await sleep(300)

        // Backend read decrypts.
        const onBackend = await exec(fetchImpl, { op: 'getLatest', collection: COLLECTION, id })
        expect(onBackend[id]).toEqual({ name: 'ada', secret: 'hunter2' })

        // At rest, the journal holds ciphertext, not the plaintext secret.
        const raw = await /** @type {any} */ (new Fylo(root)).engine.events.readSince(COLLECTION, 0)
        const journalDoc = raw.events.find((/** @type {any} */ e) => e.id === id)?.doc
        expect(journalDoc.secret).not.toBe('hunter2')

        // A second client pulls the change — and receives plaintext (the changes
        // feed decrypts before streaming).
        const b = client('http://fylo.test', fetchImpl)
        await b.ready()
        await b.sync.start()
        void b.secrets // subscribe to the changes feed
        await sleep(500)
        const localB = await b.secrets.latest(id)
        expect(localB[id]).toEqual({ name: 'ada', secret: 'hunter2' })

        a.sync.stop()
        b.sync.stop()
        await a.close()
        await b.close()
    })
})
