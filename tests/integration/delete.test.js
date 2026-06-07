import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { commentsURL, usersURL } from '../data.js'
import { createTestRoot } from '../helpers/root.js'
const COMMENTS = `comment`
const USERS = `user`
let commentsResults = {}
let usersResults = {}
const root = await createTestRoot('fylo-delete-')
const fylo = new Fylo(root)
beforeAll(async () => {
    await Promise.all([fylo[COMMENTS].create(), fylo._sql(`CREATE TABLE ${USERS}`)])
    try {
        await Promise.all([
            fylo[COMMENTS].import(new URL(commentsURL), 100),
            fylo[USERS].import(new URL(usersURL), 100)
        ])
    } catch {}
    for await (const data of fylo[COMMENTS].find({ $limit: 1 }).collect()) {
        commentsResults = { ...commentsResults, ...data }
    }
    usersResults = await fylo._sql(`SELECT * FROM ${USERS} LIMIT 1`)
})
afterAll(async () => {
    await Promise.all([fylo[COMMENTS].drop(), fylo._sql(`DROP TABLE ${USERS}`)])
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', async () => {
    test('DELETE ONE', async () => {
        const id = Object.keys(commentsResults).shift()
        try {
            await fylo[COMMENTS].delete(id)
        } catch {}
        commentsResults = {}
        for await (const data of fylo[COMMENTS].find().collect()) {
            commentsResults = { ...commentsResults, ...data }
        }
        const idx = Object.keys(commentsResults).findIndex((_id) => _id === id)
        expect(idx).toEqual(-1)
    })
    test('DELETE CLAUSE', async () => {
        try {
            await fylo[COMMENTS].deleteMany({ $ops: [{ name: { $like: '%et%' } }] })
        } catch (e) {
            console.error(e)
        }
        commentsResults = {}
        for await (const data of fylo[COMMENTS].find({
            $ops: [{ name: { $like: '%et%' } }]
        }).collect()) {
            commentsResults = { ...commentsResults, ...data }
        }
        expect(Object.keys(commentsResults).length).toEqual(0)
    })
    test('DELETE ALL', async () => {
        try {
            await fylo[COMMENTS].deleteMany()
        } catch {}
        commentsResults = {}
        for await (const data of fylo[COMMENTS].find().collect()) {
            commentsResults = { ...commentsResults, ...data }
        }
        expect(Object.keys(commentsResults).length).toEqual(0)
    })
})
describe('SQL', async () => {
    test('DELETE CLAUSE', async () => {
        const name = Object.values(usersResults).shift().name
        try {
            await fylo._sql(`DELETE FROM ${USERS} WHERE name = '${name}'`)
        } catch {}
        usersResults = await fylo._sql(`SELECT * FROM ${USERS} WHERE name = '${name}'`)
        const idx = Object.values(usersResults).findIndex((com) => com.name === name)
        expect(idx).toBe(-1)
    })
    test('DELETE ALL', async () => {
        try {
            await fylo._sql(`DELETE FROM ${USERS}`)
        } catch {}
        usersResults = await fylo._sql(`SELECT * FROM ${USERS}`)
        expect(Object.keys(usersResults).length).toBe(0)
    })
})
