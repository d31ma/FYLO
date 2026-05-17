import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { rm, unlink } from 'node:fs/promises'
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
        mode: 'append-only',
        deletePolicy: 'tombstone'
    }
})

beforeAll(async () => {
    await fylo.createCollection(BASIC_COLLECTION)
    await wormFylo.createCollection(WORM_COLLECTION)
})

afterAll(async () => {
    await fylo.dropCollection(BASIC_COLLECTION)
    await wormFylo.dropCollection(WORM_COLLECTION)
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

    test('rebuilds WORM heads, version metadata, and tombstones from retained state', async () => {
        const tombstoneFirst = await wormFylo.putData(WORM_COLLECTION, {
            id: 10002,
            title: 'Deleted v1'
        })
        const tombstoneSecond = await wormFylo.patchDoc(WORM_COLLECTION, {
            [tombstoneFirst]: {
                title: 'Deleted v2'
            }
        })
        await wormFylo.delDoc(WORM_COLLECTION, tombstoneSecond)

        const activeFirst = await wormFylo.putData(WORM_COLLECTION, {
            id: 10003,
            title: 'Active v1'
        })
        const activeSecond = await wormFylo.patchDoc(WORM_COLLECTION, {
            [activeFirst]: {
                title: 'Active v2'
            }
        })

        const activeHeadPath = path.join(
            ROOT,
            '.collections',
            WORM_COLLECTION,
            'heads',
            `${activeFirst}.json`
        )
        const activeFirstMetaPath = path.join(
            ROOT,
            '.collections',
            WORM_COLLECTION,
            'versions',
            `${activeFirst}.meta.json`
        )
        const activeSecondMetaPath = path.join(
            ROOT,
            '.collections',
            WORM_COLLECTION,
            'versions',
            `${activeSecond}.meta.json`
        )
        const tombstoneHeadPath = path.join(
            ROOT,
            '.collections',
            WORM_COLLECTION,
            'heads',
            `${tombstoneFirst}.json`
        )

        await unlink(activeHeadPath)
        await unlink(activeFirstMetaPath)
        await unlink(activeSecondMetaPath)
        await unlink(tombstoneHeadPath)
        await rm(path.join(ROOT, '.collections', WORM_COLLECTION, 'index'), {
            recursive: true,
            force: true
        })

        const rebuild = await wormFylo.rebuildCollection(WORM_COLLECTION)
        const activeLatest = await wormFylo.getLatest(WORM_COLLECTION, activeFirst)
        const activeHistory = await wormFylo.getHistory(WORM_COLLECTION, activeSecond)
        const deletedLatest = await wormFylo.getLatest(WORM_COLLECTION, tombstoneFirst)
        const deletedHistory = await wormFylo.getHistory(WORM_COLLECTION, tombstoneSecond)

        expect(rebuild.collection).toBe(WORM_COLLECTION)
        expect(rebuild.worm).toBe(true)
        expect(rebuild.headsRebuilt).toBeGreaterThanOrEqual(2)
        expect(rebuild.versionMetasRebuilt).toBeGreaterThanOrEqual(4)
        expect(rebuild.indexedDocs).toBeGreaterThanOrEqual(1)

        expect(Object.keys(activeLatest)[0]).toBe(activeSecond)
        expect(activeHistory).toHaveLength(2)
        expect(activeHistory[0].id).toBe(activeSecond)
        expect(activeHistory[0].previousVersionId).toBe(activeFirst)

        expect(deletedLatest).toEqual({})
        expect(deletedHistory).toHaveLength(2)
        expect(deletedHistory[0].id).toBe(tombstoneSecond)
        expect(deletedHistory[0].deleted).toBe(true)
        expect(deletedHistory[0].deletedAt).toBeNumber()

        const activeResults = []
        for await (const doc of wormFylo
            .findDocs(WORM_COLLECTION, {
                $ops: [{ id: { $eq: 10003 } }]
            })
            .collect()) {
            activeResults.push(doc)
        }

        const deletedResults = []
        for await (const doc of wormFylo
            .findDocs(WORM_COLLECTION, {
                $ops: [{ id: { $eq: 10002 } }]
            })
            .collect()) {
            deletedResults.push(doc)
        }

        expect(activeResults).toHaveLength(1)
        expect(Object.keys(activeResults[0])[0]).toBe(activeSecond)
        expect(deletedResults).toHaveLength(0)
    })
})
