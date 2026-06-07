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
    await Promise.all([fylo[ALBUMS].drop(), fylo[POSTS].drop()])
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', () => {
    test('TRUNCATE', async () => {
        const fylo = new Fylo(root)
        await fylo[POSTS].create()
        await fylo[POSTS].import(new URL(postsURL))
        await fylo[POSTS].deleteMany()
        const ids = []
        for await (const data of fylo[POSTS].find({ $limit: 1, $onlyIds: true }).collect()) {
            ids.push(data)
        }
        expect(ids.length).toBe(0)
    })
})
describe('SQL', () => {
    test('TRUNCATE', async () => {
        const fylo = new Fylo(root)
        await fylo._sql(`CREATE TABLE ${ALBUMS}`)
        await fylo[ALBUMS].import(new URL(albumURL))
        await fylo._sql(`DELETE FROM ${ALBUMS}`)
        const ids = await fylo._sql(`SELECT _id FROM ${ALBUMS} LIMIT 1`)
        expect(ids.length).toBe(0)
    })
})
