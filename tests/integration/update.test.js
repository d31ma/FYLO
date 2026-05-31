import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { photosURL, todosURL } from '../data.js'
import { createTestRoot } from '../helpers/root.js'
const PHOTOS = `photo`
const TODOS = `todo`
const root = await createTestRoot('fylo-update-')
const fylo = new Fylo(root)
beforeAll(async () => {
    await Promise.all([fylo.createCollection(PHOTOS), fylo.executeSQL(`CREATE TABLE ${TODOS}`)])
    try {
        await fylo.importBulkData(PHOTOS, new URL(photosURL), 100)
        await fylo.importBulkData(TODOS, new URL(todosURL), 100)
    } catch {}
})
afterAll(async () => {
    await Promise.all([fylo.dropCollection(PHOTOS), fylo.executeSQL(`DROP TABLE ${TODOS}`)])
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', async () => {
    test('UPDATE ONE', async () => {
        const ids = []
        for await (const data of fylo.findDocs(PHOTOS, { $limit: 1, $onlyIds: true }).collect()) {
            ids.push(data)
        }
        try {
            await fylo.patchDoc(PHOTOS, { [ids.shift()]: { title: 'All Mighty' } })
        } catch {}
        let results = {}
        for await (const data of fylo
            .findDocs(PHOTOS, {
                $ops: [{ title: { $eq: 'All Mighty' } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }
        expect(Object.keys(results).length).toBe(1)
    })
    test('UPDATE CLAUSE', async () => {
        let count = -1
        try {
            count = await fylo.patchDocs(PHOTOS, {
                $set: { title: 'All Mighti' },
                $where: { $ops: [{ title: { $like: '%est%' } }] }
            })
        } catch {}
        let results = {}
        for await (const data of fylo
            .findDocs(PHOTOS, {
                $ops: [{ title: { $eq: 'All Mighti' } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }
        expect(Object.keys(results).length).toBe(count)
    })
    test('UPDATE ALL', async () => {
        let count = -1
        try {
            count = await fylo.patchDocs(PHOTOS, { $set: { title: 'All Mighter' } })
        } catch {}
        let results = {}
        for await (const data of fylo
            .findDocs(PHOTOS, {
                $ops: [{ title: { $eq: 'All Mighter' } }]
            })
            .collect()) {
            results = { ...results, ...data }
        }
        expect(Object.keys(results).length).toBe(count)
    }, 20000)
})
describe('SQL', async () => {
    test('UPDATE CLAUSE', async () => {
        let count = -1
        try {
            count = await fylo.executeSQL(
                `UPDATE ${TODOS} SET title = 'All Mighty' WHERE title LIKE '%est%'`
            )
        } catch {}
        const results = await fylo.executeSQL(`SELECT * FROM ${TODOS} WHERE title = 'All Mighty'`)
        expect(Object.keys(results).length).toBe(count)
    })
    test('UPDATE ALL', async () => {
        let count = -1
        try {
            count = await fylo.executeSQL(`UPDATE ${TODOS} SET title = 'All Mightier'`)
        } catch {}
        const results = await fylo.executeSQL(`SELECT * FROM ${TODOS} WHERE title = 'All Mightier'`)
        expect(Object.keys(results).length).toBe(count)
    }, 20000)
})
