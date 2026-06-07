import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { postsURL } from '../data.js'
import { createTestRoot } from '../helpers/root.js'
const POSTS = 'exp-post'
const IMPORT_LIMIT = 20
let importedCount = 0
const root = await createTestRoot('fylo-export-')
const fylo = new Fylo(root)
beforeAll(async () => {
    await fylo[POSTS].create()
    try {
        importedCount = await fylo[POSTS].import(new URL(postsURL), IMPORT_LIMIT)
    } catch {}
})
afterAll(async () => {
    await fylo[POSTS].drop()
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', () => {
    test('EXPORT count matches import', async () => {
        let exported = 0
        for await (const _doc of fylo[POSTS].export()) {
            exported++
        }
        expect(exported).toBe(importedCount)
    })
    test('EXPORT document shape', async () => {
        for await (const doc of fylo[POSTS].export()) {
            expect(doc).toHaveProperty('title')
            expect(doc).toHaveProperty('userId')
            expect(doc).toHaveProperty('body')
            break
        }
    })
    test('EXPORT all documents are valid posts', async () => {
        for await (const doc of fylo[POSTS].export()) {
            expect(typeof doc.title).toBe('string')
            expect(typeof doc.userId).toBe('number')
            expect(doc.userId).toBeGreaterThan(0)
        }
    })
})
