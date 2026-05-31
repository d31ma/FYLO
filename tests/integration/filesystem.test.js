import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { rm, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-filesystem-')
const fylo = new Fylo(root)
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
        const activePath = path.join(
            root,
            '.collections',
            POSTS,
            'docs',
            id.slice(0, 2),
            `${id}.json`
        )
        const createdTime = (await stat(activePath)).mtimeMs
        const created = await fylo.getDoc(POSTS, id).once()
        expect(created[id].title).toBe('Hello')
        expect(created[id].tags).toEqual(['bun', 'storage'])
        await Bun.sleep(10)
        const nextId = await fylo.patchDoc(POSTS, {
            [id]: {
                title: 'Hello 2',
                meta: { score: 2 }
            }
        })
        expect(nextId).toBe(id)
        const updated = await fylo.getDoc(POSTS, nextId).once()
        expect(updated[nextId].title).toBe('Hello 2')
        expect(updated[nextId].meta.score).toBe(2)
        expect((await stat(activePath)).mtimeMs).toBeGreaterThan(createdTime)
        await fylo.delDoc(POSTS, nextId)
        expect(await fylo.getDoc(POSTS, nextId).once()).toEqual({})
        const deletedPath = path.join(
            root,
            '.collections',
            POSTS,
            '.deleted',
            id.slice(0, 2),
            `${id}.json`
        )
        expect(await Bun.file(activePath).exists()).toBe(false)
        expect(await Bun.file(deletedPath).exists()).toBe(true)
    })
    test('path constructor exposes SQL tag and collision-checked collection facades', async () => {
        const { sql, db } = new Fylo(root)
        await sql`CREATE TABLE apiusers`
        const id = await sql`INSERT INTO apiusers (name, role) VALUES (${"O'Brien"}, ${'admin'})`

        const queried = await sql`SELECT * FROM apiusers WHERE name = ${"O'Brien"}`
        expect(queried[id].role).toBe('admin')

        const viaFacade = await db.apiusers.getDoc(id).once()
        expect(viaFacade[id].name).toBe("O'Brien")
        await db.apiusers.patchDoc(id, { role: 'owner' })
        expect((await db.apiusers.getDoc(id).once())[id].role).toBe('owner')
        expect(() => db.getDoc).toThrow('reserved db property')
        expect(() => db.hasOwnProperty).toThrow('reserved db property')
        await expect(sql`SELECT * FROM apiusers WHERE name = ${{ nested: true }}`).rejects.toThrow(
            'SQL parameters must be scalar values'
        )
        expect(() => new Fylo(`fylo://${root}`)).toThrow('remove fylo://')
    })
    test('memory query cache stores TTID lists and invalidates on writes', async () => {
        const cached = new Fylo(root, { cache: true })
        const collection = 'cache-users'
        await cached.createCollection(collection)

        const firstId = await cached.db[collection].putData({ name: 'Ada', team: 'platform' })
        const query = { $ops: [{ name: { $eq: 'Ada' } }] }
        const firstResults = {}
        for await (const doc of cached.db[collection].findDocs(query).collect()) {
            Object.assign(firstResults, doc)
        }
        expect(Object.keys(firstResults)).toEqual([firstId])

        const secondId = await cached.db[collection].putData({ name: 'Ada', team: 'runtime' })
        const secondResults = {}
        for await (const doc of cached.db[collection].findDocs(query).collect()) {
            Object.assign(secondResults, doc)
        }
        expect(Object.keys(secondResults).sort()).toEqual([firstId, secondId].sort())
    })
    test('query cache stampede protection single-flights concurrent misses', async () => {
        const cached = new Fylo(root, { cache: true })
        const collection = 'cache-stampede-users'
        await cached.createCollection(collection)
        await cached.db[collection].putData({ name: 'Grace' })

        const originalListQueryableDocIds = cached.engine.listQueryableDocIds.bind(cached.engine)
        let calls = 0
        cached.engine.listQueryableDocIds = async (targetCollection) => {
            calls++
            await Bun.sleep(10)
            return await originalListQueryableDocIds(targetCollection)
        }

        await Promise.all([
            Array.fromAsync(cached.db[collection].findDocs({}).collect()),
            Array.fromAsync(cached.db[collection].findDocs({}).collect())
        ])

        expect(calls).toBe(1)
    })
    test('write-through query cache surfaces cache-version failures on writes', async () => {
        const cached = new Fylo(root, { cache: { method: 'write-through' } })
        const collection = 'cache-write-through-users'
        await cached.createCollection(collection)
        if (!cached.engine.queryCache) throw new Error('missing query cache')
        cached.engine.queryCache.bumpCollection = async () => {
            throw new Error('cache unavailable')
        }

        await expect(cached.db[collection].putData({ name: 'Linus' })).rejects.toThrow(
            'cache unavailable'
        )
    })
    test('stores collection data under .collections', async () => {
        expect(await isDirectory(path.join(root, '.collections', POSTS, 'docs'))).toBe(true)
        expect(await isDirectory(path.join(root, '.collections', POSTS, 'index'))).toBe(true)
        expect(await isDirectory(path.join(root, POSTS))).toBe(false)
    })
    test('queries updatedAt from the stable document file modification time', async () => {
        const id = await fylo.putData(POSTS, { title: 'Timestamp v1' })
        await Bun.sleep(10)
        await fylo.patchDoc(POSTS, { [id]: { title: 'Timestamp v2' } })
        const target = path.join(root, '.collections', POSTS, 'docs', id.slice(0, 2), `${id}.json`)
        const updatedAt = (await stat(target)).mtimeMs
        const results = []
        for await (const doc of fylo.findDocs(POSTS, { $updated: { $gte: updatedAt } }).collect()) {
            results.push(doc)
        }
        expect(results.some((doc) => Object.hasOwn(doc, id))).toBe(true)
    })
    test('delete listeners filter using stored document timestamps', async () => {
        const id = await fylo.putData(POSTS, { title: 'Deleted timestamp' })
        const target = path.join(root, '.collections', POSTS, 'docs', id.slice(0, 2), `${id}.json`)
        const updatedAt = (await stat(target)).mtimeMs
        const deletes = fylo
            .findDocs(POSTS, {
                $ops: [{ title: { $eq: 'Deleted timestamp' } }],
                $updated: { $lte: updatedAt }
            })
            .onDelete()
            [Symbol.asyncIterator]()
        const pending = deletes.next()

        await Bun.sleep(10)
        await fylo.delDoc(POSTS, id)

        expect((await pending).value).toBe(id)
        await deletes.return?.()
    })
    test('soft deletes are read-only and queryable by deletion time, then restorable', async () => {
        const id = await fylo.putData(POSTS, { title: 'Restore me', status: 'archived' })
        const deletedAtFloor = Date.now()

        await fylo.delDoc(POSTS, id)

        const deletedPath = path.join(
            root,
            '.collections',
            POSTS,
            '.deleted',
            id.slice(0, 2),
            `${id}.json`
        )
        const deletedMetadata = await stat(deletedPath)
        expect(deletedMetadata.mtimeMs).toBeGreaterThanOrEqual(deletedAtFloor)
        expect(deletedMetadata.mode & 0o777).toBe(0o444)

        const deleted = []
        for await (const doc of fylo
            .findDeletedDocs(POSTS, {
                $ops: [{ title: { $eq: 'Restore me' } }],
                $deleted: { $gte: deletedAtFloor }
            })
            .collect()) {
            deleted.push(doc)
        }
        expect(deleted).toEqual([{ [id]: { title: 'Restore me', status: 'archived' } }])
        await expect(
            fylo.putData(POSTS, { [id]: { title: 'Bypass restore', status: 'archived' } })
        ).rejects.toThrow('soft-deleted')

        await fylo.restoreDoc(POSTS, id)

        const activePath = path.join(
            root,
            '.collections',
            POSTS,
            'docs',
            id.slice(0, 2),
            `${id}.json`
        )
        expect(await Bun.file(deletedPath).exists()).toBe(false)
        expect((await stat(activePath)).mode & 0o777).toBe(0o644)
        expect(await fylo.getDoc(POSTS, id).once()).toEqual({
            [id]: { title: 'Restore me', status: 'archived' }
        })
        const queried = []
        for await (const doc of fylo
            .findDocs(POSTS, { $ops: [{ status: { $eq: 'archived' } }] })
            .collect()) {
            queried.push(doc)
        }
        expect(queried.some((doc) => Object.hasOwn(doc, id))).toBe(true)
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
    test('reads CRLF-terminated local index lines', async () => {
        const collection = 'filesystem-crlf-index'
        await fylo.createCollection(collection)
        const id = await fylo.putData(collection, { title: 'Windows index' })
        const walPath = path.join(root, '.collections', collection, 'index', 'keys.wal')
        const wal = await Bun.file(walPath).text()
        await writeFile(walPath, wal.replace(/\n/g, '\r\n'))

        let results = {}
        for await (const data of fylo
            .findDocs(collection, {
                $ops: [{ title: { $eq: 'Windows index' } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }

        expect(Object.keys(results)).toEqual([id])
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
        expect(secondId).toBe(firstId)

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
