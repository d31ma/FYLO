import { test, expect, describe, afterAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { postsURL, albumURL } from '../data.js'
import { createTestRoot } from '../helpers/root.js'
const POSTS = `post`
const ALBUMS = `album`
const root = await createTestRoot('fylo-truncate-')
afterAll(async () => {
    const fylo = new Fylo(root)
    await Promise.all([fylo.dropCollection(ALBUMS), fylo.dropCollection(POSTS)])
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', () => {
    test('TRUNCATE', async () => {
        const fylo = new Fylo(root)
        await fylo.createCollection(POSTS)
        await fylo.importBulkData(POSTS, new URL(postsURL))
        await fylo.delDocs(POSTS)
        const ids = []
        for await (const data of fylo.findDocs(POSTS, { $limit: 1, $onlyIds: true }).collect()) {
            ids.push(data)
        }
        expect(ids.length).toBe(0)
    })
})
describe('SQL', () => {
    test('TRUNCATE', async () => {
        const fylo = new Fylo(root)
        await fylo.executeSQL(`CREATE TABLE ${ALBUMS}`)
        await fylo.importBulkData(ALBUMS, new URL(albumURL))
        await fylo.executeSQL(`DELETE FROM ${ALBUMS}`)
        const ids = await fylo.executeSQL(`SELECT _id FROM ${ALBUMS} LIMIT 1`)
        expect(ids.length).toBe(0)
    })
})
