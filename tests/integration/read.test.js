import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { albumURL, postsURL } from '../data.js'
import { createTestRoot } from '../helpers/root.js'
const POSTS = `post`
const ALBUMS = `album`
let count = 0
const root = await createTestRoot('fylo-read-')
const fylo = new Fylo(root)
beforeAll(async () => {
    await Promise.all([fylo[ALBUMS].create(), fylo._sql(`CREATE TABLE ${POSTS}`)])
    try {
        count = await fylo[ALBUMS].import(new URL(albumURL), 100)
        await fylo[POSTS].import(new URL(postsURL), 100)
    } catch {}
})
afterAll(async () => {
    await Promise.all([fylo[ALBUMS].drop(), fylo[POSTS].drop()])
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', async () => {
    test('SELECT ALL', async () => {
        let results = {}
        for await (const data of fylo[ALBUMS].find().collect()) {
            results = { ...results, ...data }
        }
        expect(Object.keys(results).length).toBe(count)
    })
    test('SELECT PARTIAL', async () => {
        let results = {}
        for await (const data of fylo[ALBUMS].find({ $select: ['title'] }).collect()) {
            results = { ...results, ...data }
        }
        const allAlbums = Object.values(results)
        const onlyTtitle = allAlbums.every((user) => user.title && !user.userId)
        expect(onlyTtitle).toBe(true)
    })
    test('GET ONE', async () => {
        const ids = []
        for await (const data of fylo[ALBUMS].find({ $limit: 1, $onlyIds: true }).collect()) {
            ids.push(data)
        }
        const result = await fylo[ALBUMS].get(ids[0]).once()
        const _id = Object.keys(result).shift()
        expect(ids[0]).toEqual(_id)
    })
    test('SELECT CLAUSE', async () => {
        let results = {}
        for await (const data of fylo[ALBUMS].find({
            $ops: [{ userId: { $eq: 2 } }]
        }).collect()) {
            results = { ...results, ...data }
        }
        const allAlbums = Object.values(results)
        const onlyUserId = allAlbums.every((user) => user.userId === 2)
        expect(onlyUserId).toBe(true)
    })
    test('SELECT LIMIT', async () => {
        let results = {}
        for await (const data of fylo[ALBUMS].find({ $limit: 5 }).collect()) {
            results = { ...results, ...data }
        }
        expect(Object.keys(results).length).toBe(5)
    })
    test('SELECT GROUP BY', async () => {
        let results = {}
        for await (const data of fylo[ALBUMS].find({
            $groupby: 'userId',
            $onlyIds: true
        }).collect()) {
            results = Object.appendGroup(results, data)
        }
        expect(Object.keys(results).length).toBeGreaterThan(0)
    })
    test('SELECT JOIN', async () => {
        const results = await fylo.joinDocs({
            $leftCollection: ALBUMS,
            $rightCollection: POSTS,
            $mode: 'inner',
            $on: { userId: { $eq: 'id' } }
        })
        expect(Object.keys(results).length).toBeGreaterThan(0)
    })
})
describe('SQL', async () => {
    test('SELECT PARTIAL', async () => {
        const results = await fylo._sql(`SELECT title FROM ${ALBUMS}`)
        const allAlbums = Object.values(results)
        const onlyTtitle = allAlbums.every((user) => user.title && !user.userId)
        expect(onlyTtitle).toBe(true)
    })
    test('SELECT CLAUSE', async () => {
        const results = await fylo._sql(`SELECT * FROM ${ALBUMS} WHERE user_id = 2`)
        const allAlbums = Object.values(results)
        const onlyUserId = allAlbums.every((user) => user.userId === 2)
        expect(onlyUserId).toBe(true)
    })
    test('SELECT ALL', async () => {
        const results = await fylo._sql(`SELECT * FROM ${ALBUMS}`)
        expect(Object.keys(results).length).toBe(count)
    })
    test('SELECT LIMIT', async () => {
        const results = await fylo._sql(`SELECT * FROM ${ALBUMS} LIMIT 5`)
        expect(Object.keys(results).length).toBe(5)
    })
    test('SELECT GROUP BY', async () => {
        const results = await fylo._sql(`SELECT * FROM ${ALBUMS} GROUP BY userId`)
        expect(Object.keys(results).length).toBeGreaterThan(0)
    })
    test('SELECT JOIN', async () => {
        const results = await fylo._sql(
            `SELECT * FROM ${ALBUMS} INNER JOIN ${POSTS} ON userId = id`
        )
        expect(Object.keys(results).length).toBeGreaterThan(0)
    })
})
