import { afterAll, describe, expect, test } from 'bun:test'
import { mkdir, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { VersionRepository } from '../../src/versioning/repository.js'
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
        const [aExit, bExit, aStderr, bStderr] = await Promise.all([
            a.exited,
            b.exited,
            new Response(a.stderr).text(),
            new Response(b.stderr).text()
        ])
        expect(aStderr).toBe('')
        expect(bStderr).toBe('')
        expect(aExit).toBe(0)
        expect(bExit).toBe(0)
        const fylo = new Fylo(root)
        /** @type {string[]} */
        const titles = []
        for await (const data of fylo[collection].find().collect()) {
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

    test('document scans exclude durable scratch siblings from their validation set', async () => {
        const collection = 'scratch-file-scan'
        const fylo = new Fylo(root)
        await fylo[collection].create()
        const id = await fylo[collection].put({ title: 'durable record' })

        const shard = path.join(root, '.collections', collection, 'docs', 'scratch')
        await mkdir(shard, { recursive: true })
        const scratch = path.join(shard, `${id}.json.019f694e-b56b-7000-abc8-c6c912344a03.tmp`)
        await writeFile(scratch, '{}')

        const files = await fylo.engine.documents.listDocumentFiles(
            path.join(root, '.collections', collection, 'docs')
        )

        expect(files).not.toContain(scratch)
        expect((await new VersionRepository(root).status()).clean).toBe(true)
    })
})
