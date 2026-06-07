import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { albumURL, postsURL } from '../data.js'
import { createTestRoot } from '../helpers/root.js'
const POSTS = `post`
const ALBUMS = `album`
let postsCount = 0
let albumsCount = 0
const root = await createTestRoot('fylo-create-')
const fylo = new Fylo(root)
beforeAll(async () => {
    await Promise.all([fylo[POSTS].create(), fylo._sql(`CREATE TABLE ${ALBUMS}`)])
    try {
        albumsCount = await fylo[ALBUMS].import(new URL(albumURL), 100)
        postsCount = await fylo[POSTS].import(new URL(postsURL), 100)
    } catch {}
})
afterAll(async () => {
    await Promise.all([fylo[POSTS].drop(), fylo._sql(`DROP TABLE ${ALBUMS}`)])
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', async () => {
    test('PUT', async () => {
        let results = {}
        for await (const data of fylo[POSTS].find().collect()) {
            results = { ...results, ...data }
        }
        expect(Object.keys(results).length).toEqual(postsCount)
    })
})
describe('SQL', () => {
    test('INSERT', async () => {
        const results = await fylo._sql(`SELECT * FROM ${ALBUMS}`)
        expect(Object.keys(results).length).toEqual(albumsCount)
    })
})
