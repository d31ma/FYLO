import { afterAll, describe, expect, test } from 'bun:test'
import { readFile, rm } from 'node:fs/promises'
import path from 'node:path'
import { CargoArtifacts } from '../../scripts/cargo-artifacts.mjs'
import { Fylo } from '../../clients/node/fylo.mjs'
import { createBrowserClient } from '../../src/browser/client.js'
import { mockDirectoryHandle } from '../browser/helpers/fsa-mock.js'
import { createTestRoot } from '../helpers/root.js'

class NativeChangeFeedSamples {
    static collection = 'notes'

    static binary = path.resolve(
        process.env.FYLO_BINARY ??
            CargoArtifacts.path(
                'debug',
                process.platform === 'win32' ? 'fylo-rust.exe' : 'fylo-rust'
            )
    )

    /** @param {string} root @param {(fylo: Fylo) => Promise<void>} body */
    static async withNative(root, body) {
        const fylo = new Fylo(root, { binary: NativeChangeFeedSamples.binary, exclusiveRoot: true })
        await fylo.ready
        try {
            await body(fylo)
        } finally {
            await fylo.close()
        }
    }

    /** @param {string} root */
    static async browser(root) {
        const db = createBrowserClient({
            storage: { type: 'fsa', handle: mockDirectoryHandle(root), access: 'readwrite' },
            worker: false
        })
        await db.ready()
        return db
    }

    /** @param {{ id: string, kind: string }[]} changes */
    static sorted(changes) {
        return [...changes].sort((left, right) => left.id.localeCompare(right.id))
    }

    /** @param {string} root */
    static async journal(root) {
        const collection = NativeChangeFeedSamples.collection
        const file = path.join(root, '.collections', collection, 'events', `${collection}.ndjson`)
        return await readFile(file, 'utf8')
    }
}

const root = await createTestRoot('fylo-native-feed-')
const collection = NativeChangeFeedSamples.collection

afterAll(async () => {
    await rm(root, { recursive: true, force: true })
})

describe('the JavaScript change feed over a root the native engine writes', () => {
    test('reports native puts, updates and deletes once, and its own writes after them', async () => {
        const ids = []
        await NativeChangeFeedSamples.withNative(root, async (fylo) => {
            await fylo.createCollection(collection)
            ids.push(await fylo.putData(collection, { title: 'first' }))
            ids.push(await fylo.putData(collection, { title: 'second' }))
            ids.push(await fylo.putData(collection, { title: 'third' }))
        })

        const browser = await NativeChangeFeedSamples.browser(root)
        const first = await browser[collection].changes(null)
        expect(NativeChangeFeedSamples.sorted(first.changes)).toEqual(
            NativeChangeFeedSamples.sorted(ids.map((id) => ({ id, kind: 'put' })))
        )
        expect(first.more).toBe(false)
        const repeated = await browser[collection].changes(first.cursor)
        expect(repeated.changes).toEqual([])
        await browser.close()

        let added = ''
        await NativeChangeFeedSamples.withNative(root, async (fylo) => {
            await fylo.patchDoc(collection, ids[1], { title: 'second, changed' })
            await fylo.delDoc(collection, ids[2])
            added = await fylo.putData(collection, { title: 'fourth' })
        })

        const reopened = await NativeChangeFeedSamples.browser(root)
        const second = await reopened[collection].changes(first.cursor)
        expect(NativeChangeFeedSamples.sorted(second.changes)).toEqual(
            NativeChangeFeedSamples.sorted([
                { id: ids[1], kind: 'put' },
                { id: ids[2], kind: 'delete' },
                { id: added, kind: 'put' }
            ])
        )
        const third = await reopened[collection].changes(second.cursor)
        expect(third.changes).toEqual([])
        expect(third.cursor).toBe(second.cursor)

        const own = await reopened[collection].put({ title: 'from the browser' })
        const fourth = await reopened[collection].changes(second.cursor)
        expect(fourth.changes).toEqual([{ id: own, kind: 'put' }])
        await reopened.close()

        const lines = (await NativeChangeFeedSamples.journal(root)).trim().split('\n')
        expect(lines).toHaveLength(7)
    })
})
