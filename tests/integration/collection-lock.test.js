import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-colllock-')
const WORKER = path.join(import.meta.dir, 'collection-lock.worker.js')

describe('cross-process collection index lock', () => {
    afterAll(async () => {
        await rm(root, { recursive: true, force: true })
    })
    test('concurrent writers from two processes preserve every index entry', async () => {
        const collection = 'concurrent-writers'
        const perWorker = 15
        const spawnWorker = (prefix) =>
            Bun.spawn(['bun', WORKER, root, collection, prefix, String(perWorker)], {
                stdout: 'pipe',
                stderr: 'pipe'
            })
        const a = spawnWorker('A')
        const b = spawnWorker('B')
        const [aExit, bExit] = await Promise.all([a.exited, b.exited])
        expect(aExit).toBe(0)
        expect(bExit).toBe(0)
        const fylo = new Fylo(root)
        /** @type {string[]} */
        const titles = []
        for await (const data of fylo.findDocs(collection).collect()) {
            for (const doc of Object.values(data)) {
                titles.push(/** @type {any} */ (doc).title)
            }
        }
        titles.sort()
        const expected = []
        for (let i = 0; i < perWorker; i++) {
            expected.push(`A-${i}`, `B-${i}`)
        }
        expect(titles).toEqual(expected.sort())
    }, 30_000)
})
