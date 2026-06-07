import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { albumURL, postsURL } from '../data.js'
import { createTestRoot } from '../helpers/root.js'
const ALBUMS = 'jm-album'
const POSTS = 'jm-post'
const root = await createTestRoot('fylo-join-')
const fylo = new Fylo(root)
beforeAll(async () => {
    await Promise.all([fylo[ALBUMS].create(), fylo[POSTS].create()])
    try {
        await Promise.all([
            fylo[ALBUMS].import(new URL(albumURL), 100),
            fylo[POSTS].import(new URL(postsURL), 100)
        ])
    } catch {}
})
afterAll(async () => {
    await Promise.all([fylo[ALBUMS].drop(), fylo[POSTS].drop()])
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', async () => {
    test('INNER JOIN — returns only join field values', async () => {
        const results = await fylo.joinDocs({
            $leftCollection: ALBUMS,
            $rightCollection: POSTS,
            $mode: 'inner',
            $on: { userId: { $eq: 'userId' } }
        })
        const pairs = Object.values(results)
        expect(pairs.length).toBeGreaterThan(0)
        for (const pair of pairs) {
            expect(pair).toHaveProperty('userId')
            expect(typeof pair.userId).toBe('number')
        }
    })
    test('LEFT JOIN — returns full left-collection document', async () => {
        const results = await fylo.joinDocs({
            $leftCollection: ALBUMS,
            $rightCollection: POSTS,
            $mode: 'left',
            $on: { userId: { $eq: 'userId' } }
        })
        const docs = Object.values(results)
        expect(docs.length).toBeGreaterThan(0)
        for (const doc of docs) {
            expect(doc).toHaveProperty('title')
            expect(doc).toHaveProperty('userId')
        }
    })
    test('RIGHT JOIN — returns full right-collection document', async () => {
        const results = await fylo.joinDocs({
            $leftCollection: ALBUMS,
            $rightCollection: POSTS,
            $mode: 'right',
            $on: { userId: { $eq: 'userId' } }
        })
        const docs = Object.values(results)
        expect(docs.length).toBeGreaterThan(0)
        for (const doc of docs) {
            expect(doc).toHaveProperty('title')
            expect(doc).toHaveProperty('body')
            expect(doc).toHaveProperty('userId')
        }
    })
    test('OUTER JOIN — returns merged left + right document', async () => {
        const results = await fylo.joinDocs({
            $leftCollection: ALBUMS,
            $rightCollection: POSTS,
            $mode: 'outer',
            $on: { userId: { $eq: 'userId' } }
        })
        const docs = Object.values(results)
        expect(docs.length).toBeGreaterThan(0)
        for (const doc of docs) {
            expect(doc).toHaveProperty('userId')
        }
    })
    test('JOIN with $limit — respects the result cap', async () => {
        const results = await fylo.joinDocs({
            $leftCollection: ALBUMS,
            $rightCollection: POSTS,
            $mode: 'inner',
            $on: { userId: { $eq: 'userId' } },
            $limit: 5
        })
        expect(Object.keys(results).length).toBe(5)
    })
    test('JOIN with $select — only requested fields are returned', async () => {
        const results = await fylo.joinDocs({
            $leftCollection: ALBUMS,
            $rightCollection: POSTS,
            $mode: 'left',
            $on: { userId: { $eq: 'userId' } },
            $select: ['title'],
            $limit: 10
        })
        const docs = Object.values(results)
        expect(docs.length).toBeGreaterThan(0)
        for (const doc of docs) {
            expect(doc).toHaveProperty('title')
            expect(doc).not.toHaveProperty('userId')
        }
    })
    test('JOIN with $groupby — groups results by field value', async () => {
        const results = await fylo.joinDocs({
            $leftCollection: ALBUMS,
            $rightCollection: POSTS,
            $mode: 'inner',
            $on: { userId: { $eq: 'userId' } },
            $groupby: 'userId'
        })
        expect(Object.keys(results).length).toBeGreaterThan(0)
    })
    test('JOIN with $onlyIds — returns IDs only', async () => {
        const results = await fylo.joinDocs({
            $leftCollection: ALBUMS,
            $rightCollection: POSTS,
            $mode: 'inner',
            $on: { userId: { $eq: 'userId' } },
            $onlyIds: true,
            $limit: 10
        })
        expect(Array.isArray(results)).toBe(true)
        expect(results.length).toBeGreaterThan(0)
    })
})
describe('SQL', async () => {
    test('INNER JOIN', async () => {
        const results = await fylo._sql(
            `SELECT * FROM ${ALBUMS} INNER JOIN ${POSTS} ON userId = userId`
        )
        expect(Object.keys(results).length).toBeGreaterThan(0)
    })
    test('LEFT JOIN', async () => {
        const results = await fylo._sql(
            `SELECT * FROM ${ALBUMS} LEFT JOIN ${POSTS} ON userId = userId`
        )
        const docs = Object.values(results)
        expect(docs.length).toBeGreaterThan(0)
        expect(docs[0]).toHaveProperty('title')
    })
    test('RIGHT JOIN', async () => {
        const results = await fylo._sql(
            `SELECT * FROM ${ALBUMS} RIGHT JOIN ${POSTS} ON userId = userId`
        )
        const docs = Object.values(results)
        expect(docs.length).toBeGreaterThan(0)
        expect(docs[0]).toHaveProperty('body')
    })
})
