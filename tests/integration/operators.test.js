import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { albumURL } from '../data.js'
import { createTestRoot } from '../helpers/root.js'
const ALBUMS = 'ops-album'
const root = await createTestRoot('fylo-operators-')
const fylo = new Fylo(root)
beforeAll(async () => {
    await fylo.createCollection(ALBUMS)
    try {
        await fylo.importBulkData(ALBUMS, new URL(albumURL), 100)
    } catch {
        // network-dependent import; tolerate offline runs
    }
})
afterAll(async () => {
    await fylo.dropCollection(ALBUMS)
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', async () => {
    test('$ne — excludes matching value', async () => {
        let results = {}
        for await (const data of fylo
            .findDocs(ALBUMS, {
                $ops: [{ userId: { $ne: 1 } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }
        const albums = Object.values(results)
        const hasUserId1 = albums.some((a) => a.userId === 1)
        expect(hasUserId1).toBe(false)
        expect(albums.length).toBe(90)
    })
    test('$lt — returns documents where field is less than value', async () => {
        let results = {}
        for await (const data of fylo
            .findDocs(ALBUMS, {
                $ops: [{ userId: { $lt: 5 } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }
        const albums = Object.values(results)
        const allLessThan5 = albums.every((a) => a.userId < 5)
        expect(allLessThan5).toBe(true)
        expect(albums.length).toBe(40)
    })
    test('$lte — returns documents where field is less than or equal to value', async () => {
        let results = {}
        for await (const data of fylo
            .findDocs(ALBUMS, {
                $ops: [{ userId: { $lte: 5 } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }
        const albums = Object.values(results)
        const allLte5 = albums.every((a) => a.userId <= 5)
        expect(allLte5).toBe(true)
        expect(albums.length).toBe(50)
    })
    test('$gt — returns documents where field is greater than value', async () => {
        let results = {}
        for await (const data of fylo
            .findDocs(ALBUMS, {
                $ops: [{ userId: { $gt: 5 } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }
        const albums = Object.values(results)
        const allGt5 = albums.every((a) => a.userId > 5)
        expect(allGt5).toBe(true)
        expect(albums.length).toBe(50)
    })
    test('$gte — returns documents where field is greater than or equal to value', async () => {
        let results = {}
        for await (const data of fylo
            .findDocs(ALBUMS, {
                $ops: [{ userId: { $gte: 5 } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }
        const albums = Object.values(results)
        const allGte5 = albums.every((a) => a.userId >= 5)
        expect(allGte5).toBe(true)
        expect(albums.length).toBe(60)
    })
    test('$like — matches substring pattern', async () => {
        let results = {}
        for await (const data of fylo
            .findDocs(ALBUMS, {
                $ops: [{ title: { $like: '%quidem%' } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }
        const albums = Object.values(results)
        const allMatch = albums.every((a) => a.title.includes('quidem'))
        expect(allMatch).toBe(true)
        expect(albums.length).toBeGreaterThan(0)
    })
    test('$like — prefix pattern', async () => {
        let results = {}
        for await (const data of fylo
            .findDocs(ALBUMS, {
                $ops: [{ title: { $like: 'omnis%' } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }
        const albums = Object.values(results)
        const allStartWith = albums.every((a) => a.title.startsWith('omnis'))
        expect(allStartWith).toBe(true)
        expect(albums.length).toBeGreaterThan(0)
    })
})
describe('SQL', async () => {
    test('WHERE != — excludes matching value', async () => {
        const results = await fylo.executeSQL(`SELECT * FROM ${ALBUMS} WHERE userId != 1`)
        const albums = Object.values(results)
        const hasUserId1 = albums.some((a) => a.userId === 1)
        expect(hasUserId1).toBe(false)
        expect(albums.length).toBe(90)
    })
    test('WHERE LIKE — matches substring pattern', async () => {
        const results = await fylo.executeSQL(`SELECT * FROM ${ALBUMS} WHERE title LIKE '%quidem%'`)
        const albums = Object.values(results)
        const allMatch = albums.every((a) => a.title.includes('quidem'))
        expect(allMatch).toBe(true)
        expect(albums.length).toBeGreaterThan(0)
    })
})
