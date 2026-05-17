import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { access, rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const COLLECTION = 'worm-posts'
const TOMBSTONE_COLLECTION = 'worm-posts-tombstone'
const root = await createTestRoot('fylo-worm-')
const fylo = new Fylo({
    root,
    worm: {
        mode: 'append-only'
    }
})
const tombstoneFylo = new Fylo({
    root,
    worm: {
        mode: 'append-only',
        deletePolicy: 'tombstone'
    }
})

beforeAll(async () => {
    await fylo.createCollection(COLLECTION)
    await tombstoneFylo.createCollection(TOMBSTONE_COLLECTION)
})

afterAll(async () => {
    await fylo.dropCollection(COLLECTION)
    await tombstoneFylo.dropCollection(TOMBSTONE_COLLECTION)
    await rm(root, { recursive: true, force: true })
})

describe('WORM mode', () => {
    test('patchDoc keeps the old version readable while only indexing the latest head', async () => {
        const initial = {
            id: 9001,
            title: 'Original',
            body: 'v1'
        }

        const firstId = await fylo.putData(COLLECTION, initial)
        const secondId = await fylo.patchDoc(COLLECTION, {
            [firstId]: { title: 'Updated', body: 'v2' }
        })

        const firstVersion = await fylo.getDoc(COLLECTION, firstId).once()
        const secondVersion = await fylo.getDoc(COLLECTION, secondId).once()

        expect(firstVersion[firstId].title).toBe('Original')
        expect(secondVersion[secondId].title).toBe('Updated')

        const results = []
        for await (const doc of fylo
            .findDocs(COLLECTION, {
                $ops: [{ id: { $eq: 9001 } }]
            })
            .collect()) {
            results.push(doc)
        }

        expect(results).toHaveLength(1)
        expect(Object.keys(results[0])[0]).toBe(secondId)

        const headPath = path.join(root, '.collections', COLLECTION, 'heads', `${firstId}.json`)
        const versionPath = path.join(
            root,
            '.collections',
            COLLECTION,
            'versions',
            `${secondId}.meta.json`
        )

        await access(headPath)
        await access(versionPath)

        const head = await Bun.file(headPath).json()
        const version = await Bun.file(versionPath).json()

        expect(head.currentVersionId).toBe(secondId)
        expect(version.previousVersionId).toBe(firstId)
        expect(version.lineageId).toBe(firstId)
    })

    test('getLatest and getHistory resolve the active head and preserved lineage', async () => {
        const firstId = await fylo.putData(COLLECTION, {
            id: 9004,
            title: 'History A',
            body: 'v1'
        })

        const secondId = await fylo.patchDoc(COLLECTION, {
            [firstId]: {
                title: 'History B',
                body: 'v2'
            }
        })

        const latest = await fylo.getLatest(COLLECTION, firstId)
        const latestId = await fylo.getLatest(COLLECTION, firstId, true)
        const history = await fylo.getHistory(COLLECTION, secondId)

        expect(Object.keys(latest)[0]).toBe(secondId)
        expect(latestId).toBe(secondId)
        expect(history).toHaveLength(2)
        expect(history[0].id).toBe(secondId)
        expect(history[0].isHead).toBe(true)
        expect(history[0].deleted).toBe(false)
        expect(history[0].previousVersionId).toBe(firstId)
        expect(history[1].id).toBe(firstId)
        expect(history[1].lineageId).toBe(firstId)
    })

    test('versioned putData advances the logical head without deleting history', async () => {
        const firstId = await fylo.putData(COLLECTION, {
            id: 9002,
            title: 'First',
            body: 'v1'
        })

        const secondId = await fylo.putData(COLLECTION, {
            [firstId]: {
                id: 9002,
                title: 'Second',
                body: 'v2'
            }
        })

        const firstVersion = await fylo.getDoc(COLLECTION, firstId).once()
        const secondVersion = await fylo.getDoc(COLLECTION, secondId).once()

        expect(firstVersion[firstId].title).toBe('First')
        expect(secondVersion[secondId].title).toBe('Second')

        const results = []
        for await (const doc of fylo
            .findDocs(COLLECTION, {
                $ops: [{ id: { $eq: 9002 } }]
            })
            .collect()) {
            results.push(doc)
        }

        expect(results).toHaveLength(1)
        expect(Object.keys(results[0])[0]).toBe(secondId)
    })

    test('delete is rejected in WORM mode', async () => {
        const docId = await fylo.putData(COLLECTION, {
            id: 9003,
            title: 'Locked',
            body: 'retain me'
        })

        await expect(fylo.delDoc(COLLECTION, docId)).rejects.toThrow(
            'Delete is not allowed in WORM mode'
        )
    })

    test('tombstone deletes hide the active head but preserve readable history', async () => {
        const firstId = await tombstoneFylo.putData(TOMBSTONE_COLLECTION, {
            id: 9101,
            title: 'Tombstone A',
            body: 'v1'
        })

        const secondId = await tombstoneFylo.patchDoc(TOMBSTONE_COLLECTION, {
            [firstId]: {
                title: 'Tombstone B',
                body: 'v2'
            }
        })

        await tombstoneFylo.delDoc(TOMBSTONE_COLLECTION, secondId)

        const latest = await tombstoneFylo.getLatest(TOMBSTONE_COLLECTION, firstId)
        const history = await tombstoneFylo.getHistory(TOMBSTONE_COLLECTION, secondId)

        expect(latest).toEqual({})
        expect(history).toHaveLength(2)
        expect(history[0].id).toBe(secondId)
        expect(history[0].isHead).toBe(true)
        expect(history[0].deleted).toBe(true)
        expect(history[0].deletedAt).toBeNumber()

        const results = []
        for await (const doc of tombstoneFylo
            .findDocs(TOMBSTONE_COLLECTION, {
                $ops: [{ id: { $eq: 9101 } }]
            })
            .collect()) {
            results.push(doc)
        }

        expect(results).toHaveLength(0)

        const headPath = path.join(
            root,
            '.collections',
            TOMBSTONE_COLLECTION,
            'heads',
            `${firstId}.json`
        )
        const head = await Bun.file(headPath).json()

        expect(head.currentVersionId).toBe(secondId)
        expect(head.deleted).toBe(true)
        expect(head.deletedAt).toBeNumber()
    })
})
