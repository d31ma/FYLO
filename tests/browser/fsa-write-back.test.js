import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { createBrowserClient } from '../../src/browser/client.js'
import { mockDirectoryHandle } from './helpers/fsa-mock.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-fsa-write-')

afterAll(async () => {
    await rm(root, { recursive: true, force: true })
})

describe('desktop engine over a browser-written root (Explorer write mode)', () => {
    test('documents put/patched/deleted by the browser engine read back on desktop', async () => {
        // Browser engine writes directly into the real root (no overlay) —
        // exactly what the Explorer's opt-in write mode does.
        const db = createBrowserClient({
            storage: {
                type: 'fsa',
                handle: mockDirectoryHandle(root),
                access: 'readwrite'
            },
            worker: false
        })
        await db.ready()
        await db.users.create()
        const adaId = await db.users.put({ name: 'Ada', role: 'admin', age: 45 })
        const bobId = await db.users.put({ name: 'Bob', role: 'viewer', age: 30 })
        await db.users.patch(adaId, { role: 'owner' })
        await db.users.delete(bobId)
        await db.close()

        // The desktop engine opens the same root cold.
        const desktop = new Fylo(root, { versioning: { autoCommit: false } })
        const inspected = await desktop.users.inspect()
        expect(inspected.exists).toBe(true)

        const ada = (await desktop.users.get(adaId).once())[adaId]
        expect(ada).toMatchObject({ name: 'Ada', role: 'owner', age: 45 })

        // Browser soft-delete lands in .deleted/ where desktop restore finds it.
        expect((await desktop.users.get(bobId).once())[bobId]).toBeUndefined()
        await desktop.users.restore(bobId)
        expect((await desktop.users.get(bobId).once())[bobId].name).toBe('Bob')

        // Desktop queries over the browser-written data (desktop rebuilds its
        // own index from the documents — files are truth).
        await desktop.users.rebuild()
        const owners = await Array.fromAsync(
            desktop.users.find({ $ops: [{ role: { $eq: 'owner' } }] }).collect()
        )
        expect(owners.some((entry) => entry[adaId])).toBe(true)
    })
})
