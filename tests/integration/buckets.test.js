import { afterEach, describe, expect, test } from 'bun:test'
import { cp, mkdir, rename, rm, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

/** @type {string[]} */
const roots = []
afterEach(async () => {
    for (const root of roots.splice(0)) await rm(root, { recursive: true, force: true })
})
async function freshRoot() {
    const root = await createTestRoot('fylo-buckets-')
    roots.push(root)
    return root
}
const exists = (p) =>
    stat(p).then(
        () => true,
        () => false
    )

describe('file collections are buckets under .buckets/', () => {
    test('new file collections write to .buckets, documents to .collections', async () => {
        const root = await freshRoot()
        const db = new Fylo(root, { versioning: { autoCommit: false } })
        await db.users.create() // document collection
        await db.assets.create({ kind: 'file' }) // bucket
        const docId = await db.users.put({ name: 'Ada' })
        const fileId = await db.assets.put(new File(['bytes'], 'a.txt'), { key: '/a.txt' })

        expect(await exists(path.join(root, '.collections', 'users', 'docs'))).toBe(true)
        expect(await exists(path.join(root, '.collections', 'assets'))).toBe(false)
        expect(await exists(path.join(root, '.buckets', 'assets', 'docs'))).toBe(true)
        expect(await exists(path.join(root, '.buckets', 'users'))).toBe(false)

        // both remain readable through the unified API
        expect((await db.users.get(docId).once())[docId].name).toBe('Ada')
        expect(new TextDecoder().decode(await db.assets.get(fileId).bytes())).toBe('bytes')
    })

    test('auto-migrates a legacy .collections file collection to .buckets on open', async () => {
        const root = await freshRoot()
        // Seed a legacy layout: a file collection physically under .collections/
        // (as older FYLO versions wrote it), with its kind:'file' descriptor.
        const seed = new Fylo(root, { versioning: { autoCommit: false } })
        await seed.legacy.create({ kind: 'file' })
        const fileId = await seed.legacy.put(new File(['old bytes'], 'old.txt'), {
            key: '/old.txt'
        })
        // Move it back to the pre-migration location by hand.
        await mkdir(path.join(root, '.collections'), { recursive: true })
        await rename(
            path.join(root, '.buckets', 'legacy'),
            path.join(root, '.collections', 'legacy')
        )
        await rm(path.join(root, '.buckets'), { recursive: true, force: true })
        expect(await exists(path.join(root, '.collections', 'legacy', 'docs'))).toBe(true)

        // A fresh engine opens it: first access migrates to .buckets, data intact.
        const reopened = new Fylo(root, { versioning: { autoCommit: false } })
        expect(new TextDecoder().decode(await reopened.legacy.get(fileId).bytes())).toBe(
            'old bytes'
        )
        expect(await exists(path.join(root, '.buckets', 'legacy', 'docs'))).toBe(true)
        expect(await exists(path.join(root, '.collections', 'legacy'))).toBe(false)
    })

    test('versioning commits and restores a bucket', async () => {
        const root = await freshRoot()
        const db = new Fylo(root) // autoCommit on
        const { VersionRepository } = await import('../../src/versioning/repository.js')
        const repo = new VersionRepository(root)
        await db.docs.create({ kind: 'file' })
        const id = await db.docs.put(new File(['v1'], 'f.txt'), { key: '/f.txt' })
        const [head] = await repo.log({ limit: 1 })
        expect(head?.id).toBeString()

        await db.docs.delete(id)
        await repo.restoreCommit(head.id)
        const restored = new Fylo(root)
        expect(new TextDecoder().decode(await restored.docs.get(id).bytes())).toBe('v1')
    })
})
