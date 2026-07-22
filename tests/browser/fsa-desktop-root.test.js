import { afterAll, describe, expect, test } from 'bun:test'
import { readdir, rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { createBrowserClient } from '../../src/browser/client.js'
import { mockDirectoryHandle } from './helpers/fsa-mock.js'
import { createTestRoot } from '../helpers/root.js'

async function snapshotTree(dir) {
    const entries = await readdir(dir, { recursive: true })
    return entries.sort().join('\n')
}

async function collect(cursor) {
    const out = {}
    for await (const page of cursor.collect()) Object.assign(out, page)
    return out
}

const root = await createTestRoot('fylo-fsa-')

afterAll(async () => {
    await rm(root, { recursive: true, force: true })
})

describe('browser engine over a desktop-written root (FSA adapter)', () => {
    test('reads, queries, and stays strictly read-only through the overlay', async () => {
        // Write the root with the desktop engine.
        const desktop = new Fylo(root, { versioning: { autoCommit: false } })
        await desktop.users.create()
        const adaId = await desktop.users.put({ name: 'Ada', role: 'admin', age: 45 })
        await desktop.users.put({ name: 'Bob', role: 'viewer', age: 30 })

        const before = await snapshotTree(root)

        // Open it with the browser engine via the FSA adapter + overlay.
        const db = createBrowserClient({
            storage: {
                type: 'fsa',
                handle: mockDirectoryHandle(root),
                access: 'overlay'
            },
            worker: false
        })
        await db.ready()

        const inspected = await db.users.inspect()
        expect(inspected.exists).toBe(true)
        expect(inspected.docsStored).toBe(2)

        const manifest = await db.users.get(adaId).once()
        expect(manifest[adaId].name).toBe('Ada')

        // Indexes are accelerators: rebuild into the overlay (RAM), then query.
        await db.users.rebuild()
        const admins = await collect(
            db.users.find({ $ops: [{ role: { $eq: 'admin' } }, { age: { $gte: 40 } }] })
        )
        expect(Object.values(admins).map((doc) => doc.name)).toEqual(['Ada'])

        // The real root is byte-for-byte untouched.
        expect(await snapshotTree(root)).toBe(before)
        await db.close()
    })
})
