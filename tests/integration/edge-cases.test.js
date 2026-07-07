import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import TTID from '../helpers/ttid.js'
import { createTestRoot } from '../helpers/root.js'
const COLLECTION = 'ec-test'
const root = await createTestRoot('fylo-edge-')
const fylo = new Fylo(root)
beforeAll(async () => {
    await fylo[COLLECTION].create()
})
afterAll(async () => {
    await fylo[COLLECTION].drop()
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', () => {
    test('GET ONE — non-existent ID returns empty object', async () => {
        const fakeId = TTID.generate()
        const result = await fylo[COLLECTION].get(fakeId).once()
        expect(Object.keys(result).length).toBe(0)
    })
    test('GET/DELETE reject invalid document IDs before filesystem access', async () => {
        // ID validation is async now (driven by the ttid binary), so get() rejects
        // when consumed rather than throwing synchronously — but still before any
        // filesystem access.
        await expect(fylo[COLLECTION].get('../not-a-ttid').once()).rejects.toThrow(
            'Invalid document ID'
        )
        await expect(fylo[COLLECTION].delete('../not-a-ttid')).rejects.toThrow(
            'Invalid document ID'
        )
    })
    test('PUT / GET — forward slashes in values round-trip correctly', async () => {
        const original = {
            userId: 1,
            id: 1,
            title: 'Slash Test',
            body: 'https://example.com/api/v1/resource'
        }
        const _id = await fylo[COLLECTION].put(original)
        const result = await fylo[COLLECTION].get(_id).once()
        const doc = result[_id]
        expect(doc.body).toBe(original.body)
        await fylo[COLLECTION].delete(_id)
    })
    test('PUT / GET — values with multiple consecutive slashes round-trip correctly', async () => {
        const original = {
            userId: 1,
            id: 2,
            title: 'Double Slash',
            body: 'https://cdn.example.com//assets//image.png'
        }
        const _id = await fylo[COLLECTION].put(original)
        const result = await fylo[COLLECTION].get(_id).once()
        expect(result[_id].body).toBe(original.body)
        await fylo[COLLECTION].delete(_id)
    })
    test('$ops — multiple conditions act as OR union', async () => {
        const cleanFylo = new Fylo(root)
        const id1 = await cleanFylo[COLLECTION].put({
            userId: 10,
            id: 100,
            title: 'Alpha',
            body: 'first'
        })
        const id2 = await cleanFylo[COLLECTION].put({
            userId: 20,
            id: 200,
            title: 'Beta',
            body: 'second'
        })
        const results = {}
        for await (const data of fylo[COLLECTION].find({
            $ops: [{ userId: { $eq: 10 } }, { userId: { $eq: 20 } }]
        }).collect()) {
            Object.assign(results, data)
        }
        expect(results[id1]).toBeDefined()
        expect(results[id2]).toBeDefined()
        await cleanFylo[COLLECTION].delete(id1)
        await cleanFylo[COLLECTION].delete(id2)
    })
    test('$rename — renames fields in query output', async () => {
        const cleanFylo = new Fylo(root)
        const _id = await cleanFylo[COLLECTION].put({
            userId: 1,
            id: 300,
            title: 'Rename Me',
            body: 'some body'
        })
        let renamed = {}
        for await (const data of fylo[COLLECTION].find({
            $ops: [{ id: { $eq: 300 } }],
            $rename: { title: 'name' }
        }).collect()) {
            renamed = Object.values(data)[0]
        }
        expect(renamed.name).toBe('Rename Me')
        expect(renamed.title).toBeUndefined()
        await cleanFylo[COLLECTION].delete(_id)
    })
    test('keyed putData updates the original TTID file', async () => {
        const cleanFylo = new Fylo(root)
        const _id1 = await cleanFylo[COLLECTION].put({
            userId: 1,
            id: 400,
            title: 'Original',
            body: 'v1'
        })
        const _id2 = await cleanFylo[COLLECTION].put({
            [_id1]: { userId: 1, id: 400, title: 'Updated', body: 'v2' }
        })
        expect(_id2).toBe(_id1)
        const result = await fylo[COLLECTION].get(_id2).once()
        const doc = result[_id2]
        expect(doc).toBeDefined()
        expect(doc.title).toBe('Updated')
        await cleanFylo[COLLECTION].delete(_id1)
    })
    test('keyed putData does not create a second document identity', async () => {
        const cleanFylo = new Fylo(root)
        const _id1 = await cleanFylo[COLLECTION].put({
            userId: 1,
            id: 500,
            title: 'Old Version',
            body: 'original'
        })
        const _id2 = await cleanFylo[COLLECTION].put({
            [_id1]: { userId: 1, id: 500, title: 'New Version', body: 'updated' }
        })
        expect(_id1).toBe(_id2)
        await cleanFylo[COLLECTION].delete(_id2)
    })
})
describe('SQL', () => {
    test('UPDATE ONE — update a single document by querying its unique field', async () => {
        const cleanFylo = new Fylo(root)
        await cleanFylo[COLLECTION].put({
            userId: 1,
            id: 600,
            title: 'Before SQL Update',
            body: 'original'
        })
        const updated = await cleanFylo._sql(
            `UPDATE ${COLLECTION} SET title = 'After SQL Update' WHERE id = 600`
        )
        expect(updated).toBe(1)
        const results = await cleanFylo._sql(
            `SELECT * FROM ${COLLECTION} WHERE title = 'After SQL Update'`
        )
        expect(Object.keys(results).length).toBe(1)
        expect(Object.values(results)[0].title).toBe('After SQL Update')
    })
    test('DELETE ONE — delete a single document by querying its unique field', async () => {
        const cleanFylo = new Fylo(root)
        await cleanFylo[COLLECTION].put({
            userId: 1,
            id: 700,
            title: 'Delete Via SQL',
            body: 'should be removed'
        })
        await cleanFylo._sql(`DELETE FROM ${COLLECTION} WHERE title = 'Delete Via SQL'`)
        const results = await cleanFylo._sql(
            `SELECT * FROM ${COLLECTION} WHERE title = 'Delete Via SQL'`
        )
        expect(Object.keys(results).length).toBe(0)
    })
})
