import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const runPerf = process.env.FYLO_RUN_PERF_TESTS === 'true'

describe.skipIf(!runPerf)('filesystem engine performance', () => {
    /** @type {string} */
    let root
    const collection = 'filesystem-perf'
    /** @type {Fylo} */
    let fylo

    beforeAll(async () => {
        root = await createTestRoot('fylo-filesystem-perf-')
        fylo = new Fylo({ root })
        await fylo.createCollection(collection)
    })

    afterAll(async () => {
        await rm(root, { recursive: true, force: true })
    })

    test('keeps prefix index queries fast as collections grow', async () => {
        const totalDocs = 2000
        const insertStart = performance.now()

        for (let index = 0; index < totalDocs; index++) {
            await fylo.putData(collection, {
                title: `doc-${index}`,
                group: index % 10,
                tags: [`tag-${index % 5}`, `batch-${Math.floor(index / 100)}`],
                meta: { score: index }
            })
        }

        const insertMs = performance.now() - insertStart

        const exactStart = performance.now()
        let exactResults = {}
        for await (const data of fylo
            .findDocs(collection, {
                $ops: [{ title: { $eq: 'doc-1555' } }]
            })
            .collect()) {
            exactResults = { ...exactResults, ...data }
        }
        const exactMs = performance.now() - exactStart

        const rangeStart = performance.now()
        let rangeCount = 0
        for await (const data of fylo
            .findDocs(collection, {
                $ops: [{ ['meta.score']: { $gte: 1900 } }]
            })
            .collect()) {
            rangeCount += Object.keys(data).length
        }
        const rangeMs = performance.now() - rangeStart

        const indexRoot = path.join(root, '.collections', collection, 'index')
        const indexBytes = (await Bun.file(path.join(indexRoot, 'keys.wal')).text()).length

        expect(Object.keys(exactResults)).toHaveLength(1)
        expect(rangeCount).toBe(100)
        expect(indexBytes).toBeGreaterThan(0)

        console.log(
            `[FYLO perf] docs=${totalDocs} insertMs=${insertMs.toFixed(1)} exactMs=${exactMs.toFixed(
                1
            )} rangeMs=${rangeMs.toFixed(1)} indexBytes=${indexBytes}`
        )
    }, 120_000)
})
