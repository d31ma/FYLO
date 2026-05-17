import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { rm, stat } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-filesystem-')
const fylo = new Fylo({ root })
const POSTS = 'filesystem-posts'
const USERS = 'filesystem-users'

async function isDirectory(target) {
    try {
        return (await stat(target)).isDirectory()
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code === 'ENOENT') return false
        throw err
    }
}

async function readLocalIndex(collection) {
    const indexRoot = path.join(root, '.collections', collection, 'index')
    const snapshot = await Bun.file(path.join(indexRoot, 'keys.snapshot')).text()
    const wal = await Bun.file(path.join(indexRoot, 'keys.wal')).text()
    return `${snapshot}${wal}`
}

describe('filesystem engine', () => {
    beforeAll(async () => {
        await fylo.createCollection(POSTS)
        await fylo.createCollection(USERS)
    })
    afterAll(async () => {
        await rm(root, { recursive: true, force: true })
    })
    test('put/get/patch/delete works without Redis or cloud adapters', async () => {
        const id = await fylo.putData(POSTS, {
            title: 'Hello',
            tags: ['bun', 'storage'],
            meta: { score: 1 }
        })
        const created = await fylo.getDoc(POSTS, id).once()
        expect(created[id].title).toBe('Hello')
        expect(created[id].tags).toEqual(['bun', 'storage'])
        const nextId = await fylo.patchDoc(POSTS, {
            [id]: {
                title: 'Hello 2',
                meta: { score: 2 }
            }
        })
        const updated = await fylo.getDoc(POSTS, nextId).once()
        expect(updated[nextId].title).toBe('Hello 2')
        expect(updated[nextId].meta.score).toBe(2)
        expect(await fylo.getDoc(POSTS, id).once()).toEqual({})
        await fylo.delDoc(POSTS, nextId)
        expect(await fylo.getDoc(POSTS, nextId).once()).toEqual({})
    })
    test('stores collection data under .collections', async () => {
        expect(await isDirectory(path.join(root, '.collections', POSTS, 'docs'))).toBe(true)
        expect(await isDirectory(path.join(root, '.collections', POSTS, 'index'))).toBe(true)
        expect(await isDirectory(path.join(root, POSTS))).toBe(false)
    })
    test('findDocs listener is backed by the filesystem event journal', async () => {
        const iter = fylo
            .findDocs(POSTS, {
                $ops: [{ title: { $eq: 'Live event' } }]
            })
            [Symbol.asyncIterator]()
        const pending = iter.next()
        await Bun.sleep(100)
        const id = await fylo.putData(POSTS, { title: 'Live event' })
        const { value } = await pending
        expect(value).toEqual({ [id]: { title: 'Live event' } })
        await iter.return?.()
    })
    test('supports long values without path-length issues', async () => {
        const longBody = 'x'.repeat(5000)
        const id = await fylo.putData(POSTS, {
            title: 'Long payload',
            body: longBody
        })
        const result = await fylo.getDoc(POSTS, id).once()
        expect(result[id].body).toBe(longBody)
    })
    test('importBulkData rejects oversized responses and private-network URLs by default', async () => {
        const tooLarge = new URL('data:application/json,%5B%7B%22title%22%3A%22x%22%7D%5D')
        await expect(fylo.importBulkData(POSTS, tooLarge, { maxBytes: 4 })).rejects.toThrow(
            'exceeded'
        )
        await expect(
            fylo.importBulkData(POSTS, new URL('http://127.0.0.1/data.json'))
        ).rejects.toThrow('private address')
    })
    test('importBulkData reports malformed JSON with a sanitized error', async () => {
        const invalidJson = new URL('data:application/json,%5Bnot-json')
        await expect(fylo.importBulkData(POSTS, invalidJson)).rejects.toThrow(
            'Invalid JSON in import response'
        )
    })
    test('stores only user document data in the file body', async () => {
        const id = await fylo.putData(POSTS, {
            title: 'Lean doc',
            body: 'payload only'
        })
        const raw = await Bun.file(
            path.join(root, '.collections', POSTS, 'docs', id.slice(0, 2), `${id}.json`)
        ).json()

        expect(raw).toEqual({
            title: 'Lean doc',
            body: 'payload only'
        })
        expect(raw.id).toBeUndefined()
        expect(raw.createdAt).toBeUndefined()
        expect(raw.updatedAt).toBeUndefined()
    })
    test('stores query indexes as compact local object keys', async () => {
        const id = await fylo.putData(POSTS, {
            title: 'Prefix doc',
            tags: ['bun', 'prefix']
        })
        const index = await readLocalIndex(POSTS)
        expect(index).toContain(`+\ttitle/eq/Prefix%20doc/${id}`)
        expect(index).toContain(`+\ttags/eq/prefix/${id}`)
        expect(await Bun.file(path.join(root, '.collections', POSTS, 'index.db')).exists()).toBe(
            false
        )
        expect(
            await Bun.file(
                path.join(root, '.collections', POSTS, 'index', 'manifest.json')
            ).exists()
        ).toBe(true)
    })
    test('uses prefix indexes to support exact, range, like, and contains queries', async () => {
        const queryCollection = 'filesystem-query'
        await fylo.createCollection(queryCollection)

        const bunId = await fylo.putData(queryCollection, {
            title: 'Bun launch',
            tags: ['bun', 'storage'],
            meta: { score: 10 }
        })
        const nodeId = await fylo.putData(queryCollection, {
            title: 'Node launch',
            tags: ['node'],
            meta: { score: 2 }
        })

        let eqResults = {}
        for await (const data of fylo
            .findDocs(queryCollection, {
                $ops: [{ title: { $eq: 'Bun launch' } }]
            })
            .collect()) {
            eqResults = { ...eqResults, ...data }
        }
        expect(Object.keys(eqResults)).toEqual([bunId])

        let rangeResults = {}
        for await (const data of fylo
            .findDocs(queryCollection, {
                $ops: [{ ['meta.score']: { $gte: 5 } }]
            })
            .collect()) {
            rangeResults = { ...rangeResults, ...data }
        }
        expect(Object.keys(rangeResults)).toEqual([bunId])

        let containsResults = {}
        for await (const data of fylo
            .findDocs(queryCollection, {
                $ops: [{ tags: { $contains: 'storage' } }]
            })
            .collect()) {
            containsResults = { ...containsResults, ...data }
        }
        expect(Object.keys(containsResults)).toEqual([bunId])
        expect(containsResults[nodeId]).toBeUndefined()

        let prefixResults = {}
        for await (const data of fylo
            .findDocs(queryCollection, {
                $ops: [{ title: { $like: 'Bun%' } }]
            })
            .collect()) {
            prefixResults = { ...prefixResults, ...data }
        }
        expect(Object.keys(prefixResults)).toEqual([bunId])

        let suffixResults = {}
        for await (const data of fylo
            .findDocs(queryCollection, {
                $ops: [{ title: { $like: '%launch' } }]
            })
            .collect()) {
            suffixResults = { ...suffixResults, ...data }
        }
        expect(Object.keys(suffixResults).sort()).toEqual([bunId, nodeId].sort())

        let containsLikeResults = {}
        for await (const data of fylo
            .findDocs(queryCollection, {
                $ops: [{ title: { $like: '%un l%' } }]
            })
            .collect()) {
            containsLikeResults = { ...containsLikeResults, ...data }
        }
        expect(Object.keys(containsLikeResults)).toEqual([bunId])

        const index = await readLocalIndex(queryCollection)
        expect(index).toContain(`+\ttitle/eq/Bun%20launch/${bunId}`)
        expect(index).toContain(`+\ttags/eq/storage/${bunId}`)
        expect(index).toContain(`+\tmeta/score/n/`)
    })
    test('array index entries shrink and expand with document changes', async () => {
        const collection = 'filesystem-array-index'
        await fylo.createCollection(collection)
        const firstId = await fylo.putData(collection, { tags: ['alpha', 'beta'] })
        const secondId = await fylo.patchDoc(collection, { [firstId]: { tags: ['beta', 'gamma'] } })

        let oldResults = {}
        for await (const data of fylo
            .findDocs(collection, {
                $ops: [{ tags: { $contains: 'alpha' } }]
            })
            .collect()) {
            oldResults = { ...oldResults, ...data }
        }

        let newResults = {}
        for await (const data of fylo
            .findDocs(collection, {
                $ops: [{ tags: { $contains: 'gamma' } }]
            })
            .collect()) {
            newResults = { ...newResults, ...data }
        }

        expect(oldResults).toEqual({})
        expect(Object.keys(newResults)).toEqual([secondId])
    })
    test('joins work in filesystem mode', async () => {
        const userId = await fylo.putData(USERS, { id: 42, name: 'Ada' })
        const postId = await fylo.putData(POSTS, { id: 42, title: 'Shared', content: 'join me' })
        const joined = await fylo.joinDocs({
            $leftCollection: USERS,
            $rightCollection: POSTS,
            $mode: 'inner',
            $on: {
                id: { $eq: 'id' }
            }
        })
        expect(joined[`${userId}, ${postId}`]).toBeDefined()
    })
    test('rejects collection names that are unsafe for cross-platform filesystems', async () => {
        await expect(fylo.createCollection('bad/name')).rejects.toThrow('Invalid collection name')
        await expect(fylo.createCollection('bad\\name')).rejects.toThrow('Invalid collection name')
        await expect(fylo.createCollection('bad:name')).rejects.toThrow('Invalid collection name')
    })
    test('static helpers can use filesystem root env defaults', async () => {
        const prevFyloRoot = process.env.FYLO_ROOT
        process.env.FYLO_ROOT = root
        const collection = 'filesystem-static'
        await Fylo.createCollection(collection)
        const id = await fylo.putData(collection, { title: 'Static path' })
        const result = await Fylo.getDoc(collection, id).once()
        expect(result[id].title).toBe('Static path')
        if (prevFyloRoot === undefined) delete process.env.FYLO_ROOT
        else process.env.FYLO_ROOT = prevFyloRoot
    })
})
