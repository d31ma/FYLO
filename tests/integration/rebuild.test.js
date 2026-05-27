import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const ROOT = await createTestRoot('fylo-rebuild-')
const BASIC_COLLECTION = 'rebuild-posts'
const WORM_COLLECTION = 'rebuild-worm-posts'

const fylo = new Fylo({ root: ROOT })
const wormFylo = new Fylo({
    root: ROOT,
    worm: {
        mode: 'strict'
    }
})

beforeAll(async () => {
    await fylo.createCollection(BASIC_COLLECTION)
    await wormFylo.createCollection(WORM_COLLECTION)
})

afterAll(async () => {
    await fylo.dropCollection(BASIC_COLLECTION)
    await rm(ROOT, { recursive: true, force: true })
})

describe('rebuildCollection', () => {
    test('restores queryability after non-WORM index drift', async () => {
        await fylo.putData(BASIC_COLLECTION, {
            id: 10001,
            title: 'Rebuild me'
        })

        await rm(path.join(ROOT, '.collections', BASIC_COLLECTION, 'index'), {
            recursive: true,
            force: true
        })

        const before = []
        for await (const doc of fylo
            .findDocs(BASIC_COLLECTION, {
                $ops: [{ id: { $eq: 10001 } }]
            })
            .collect()) {
            before.push(doc)
        }
        expect(before).toHaveLength(0)

        const rebuild = await fylo.rebuildCollection(BASIC_COLLECTION)

        const after = []
        for await (const doc of fylo
            .findDocs(BASIC_COLLECTION, {
                $ops: [{ id: { $eq: 10001 } }]
            })
            .collect()) {
            after.push(doc)
        }

        expect(rebuild.collection).toBe(BASIC_COLLECTION)
        expect(rebuild.worm).toBe(false)
        expect(rebuild.docsScanned).toBeGreaterThanOrEqual(1)
        expect(rebuild.indexedDocs).toBeGreaterThanOrEqual(1)
        expect(after).toHaveLength(1)
    })

    test('rebuilds strict WORM indexes without introducing version metadata', async () => {
        const activeId = await wormFylo.putData(WORM_COLLECTION, {
            id: 10003,
            title: 'Immutable'
        })
        await rm(path.join(ROOT, '.collections', WORM_COLLECTION, 'index'), {
            recursive: true,
            force: true
        })

        const rebuild = await wormFylo.rebuildCollection(WORM_COLLECTION)
        const activeLatest = await wormFylo.getLatest(WORM_COLLECTION, activeId)

        expect(rebuild.collection).toBe(WORM_COLLECTION)
        expect(rebuild.worm).toBe(true)
        expect(rebuild.indexedDocs).toBeGreaterThanOrEqual(1)

        expect(Object.keys(activeLatest)[0]).toBe(activeId)
        expect(
            await Bun.file(path.join(ROOT, '.collections', WORM_COLLECTION, 'heads')).exists()
        ).toBe(false)
        expect(
            await Bun.file(path.join(ROOT, '.collections', WORM_COLLECTION, 'versions')).exists()
        ).toBe(false)

        const activeResults = []
        for await (const doc of wormFylo
            .findDocs(WORM_COLLECTION, {
                $ops: [{ id: { $eq: 10003 } }]
            })
            .collect()) {
            activeResults.push(doc)
        }

        expect(activeResults).toHaveLength(1)
        expect(Object.keys(activeResults[0])[0]).toBe(activeId)
    })
})
