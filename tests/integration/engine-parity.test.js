import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { createBrowserClient } from '../../src/browser/index.js'
import { createTestRoot } from '../helpers/root.js'

// Drift guard: `src/` (Bun engine) and `src/browser/` (OPFS engine) are two
// implementations of the same contract. This runs an identical op-sequence
// against both and asserts they produce identical observable results (document
// ids are engine-generated, so results are compared by value, not by id).

const roots = []

/** @param {Record<string, any>} obj */
const valuesSorted = (obj) =>
    Object.values(obj)
        .map((d) => JSON.stringify(d))
        .sort()

/** @param {any} cursor */
async function collect(cursor) {
    /** @type {Record<string, any>} */
    const docs = {}
    for await (const value of cursor.collect()) {
        if (value && typeof value === 'object' && !Array.isArray(value)) Object.assign(docs, value)
    }
    return docs
}

/** Runs a fixed sequence and returns an id-independent transcript. @param {any} fylo */
async function transcript(fylo) {
    const c = 'parity-people'
    await fylo[c].create()
    const alice = await fylo[c].put({ name: 'Alice', role: 'admin', score: 42, tags: ['a', 'b'] })
    const bob = await fylo[c].put({ name: 'Bob', role: 'member', score: 12 })

    /** @type {[string, any][]} */
    const out = []
    out.push(['latest', Object.values(await fylo[c].latest(alice))[0]])
    out.push([
        'eq',
        valuesSorted(await collect(fylo[c].find({ $ops: [{ role: { $eq: 'admin' } }] })))
    ])
    out.push([
        'gte',
        valuesSorted(await collect(fylo[c].find({ $ops: [{ score: { $gte: 20 } }] })))
    ])
    out.push([
        'like',
        valuesSorted(await collect(fylo[c].find({ $ops: [{ name: { $like: 'Al%' } }] })))
    ])
    out.push(['all', valuesSorted(await collect(fylo[c].find({})))])

    await fylo[c].patch(alice, { role: 'owner', score: 50 })
    out.push(['patched', Object.values(await fylo[c].latest(alice))[0]])

    // Both engines now expose `_sql` (raw string) and the `.sql` template tag.
    const sqlRows = await fylo._sql("SELECT * FROM parity-people WHERE role = 'member'")
    out.push(['sql', valuesSorted(sqlRows)])

    await fylo[c].delete(bob)
    out.push(['after-delete', valuesSorted(await collect(fylo[c].find({})))])

    return out
}

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('server ↔ browser engine parity', () => {
    test('both engines produce identical observable results for the same ops', async () => {
        const root = await createTestRoot('fylo-parity-')
        roots.push(root)
        const server = new Fylo(root)
        const browser = createBrowserClient({ storage: 'memory' })

        const serverTranscript = await transcript(server)
        const browserTranscript = await transcript(browser)

        expect(browserTranscript).toEqual(serverTranscript)

        await browser.close?.()
    })
})
