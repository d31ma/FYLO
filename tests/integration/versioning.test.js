import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { validateAgainstHead } from '../../src/schema/validation.js'
import { _resetCaches } from '../../src/schema/versioning.js'
import { createTestRoot } from '../helpers/root.js'

const COLLECTION = 'article'
const root = await createTestRoot('fylo-versioning-')
const fylo = new Fylo(root)
const originalStrict = Fylo.STRICT

beforeAll(async () => {
    await fylo.createCollection(COLLECTION)
})

afterAll(async () => {
    Fylo.STRICT = originalStrict
    try {
        await fylo.dropCollection(COLLECTION)
    } catch {}
    await rm(root, { recursive: true, force: true })
})

describe('schema versioning', () => {
    test('read materializes pre-versioning docs to head shape', async () => {
        // STRICT off → write bypasses validateAgainstHead, doc lands on disk
        // without _v field, simulating a legacy/pre-bump record.
        Fylo.STRICT = undefined
        const id = await fylo.putData(COLLECTION, {
            id: 1,
            title: 'Hello World',
            body: 'first article'
        })

        const latest = await fylo.getLatest(COLLECTION, id)
        const doc = latest[id]
        expect(doc).toBeDefined()
        expect(doc.title).toBe('Hello World')
        expect(doc.body).toBe('first article')
        // Upgrader derived slug from title; head version stamped.
        expect(doc.slug).toBe('hello-world')
        expect(doc._v).toBe('v2')
    })

    test('strict write stamps _v=head and chex validates against head schema', async () => {
        Fylo.STRICT = '1'
        const id = await fylo.putData(COLLECTION, {
            id: 2,
            title: 'Strict Insert',
            body: 'body',
            slug: 'strict-insert'
        })

        const latest = await fylo.getLatest(COLLECTION, id)
        expect(latest[id]._v).toBe('v2')
        expect(latest[id].slug).toBe('strict-insert')
    })

    test('strict write rejects docs missing fields added in head version', async () => {
        Fylo.STRICT = '1'
        // v2 head requires `slug`; chex must reject a write missing it.
        await expect(
            fylo.putData(COLLECTION, {
                id: 99,
                title: 'No Slug',
                body: 'should fail'
            })
        ).rejects.toThrow()
    })

    test('findDocs upgrades pre-versioning docs in query results', async () => {
        const seen = []
        for await (const result of fylo.findDocs(COLLECTION).collect()) {
            const [, data] = Object.entries(result)[0]
            seen.push(data)
        }
        expect(seen.length).toBeGreaterThanOrEqual(2)
        for (const doc of seen) {
            expect(doc._v).toBe('v2')
            expect(typeof doc.slug).toBe('string')
            expect(doc.slug.length).toBeGreaterThan(0)
        }
    })

    test('patch self-heals legacy doc to head shape', async () => {
        Fylo.STRICT = undefined
        const id = await fylo.putData(COLLECTION, {
            id: 3,
            title: 'Old Article',
            body: 'old body'
        })

        // Patch under STRICT: existing doc is read materialized (slug populated),
        // then merged with the patch, then validated. The on-disk new version
        // should be head-shaped.
        Fylo.STRICT = '1'
        const newId = await fylo.patchDoc(COLLECTION, {
            [id]: { title: 'Updated Title', slug: 'updated-title' }
        })

        const latest = await fylo.getLatest(COLLECTION, newId)
        expect(latest[newId]._v).toBe('v2')
        expect(latest[newId].title).toBe('Updated Title')
        expect(latest[newId].slug).toBe('updated-title')
        expect(latest[newId].body).toBe('old body')
    })

    test('doc at unknown version throws on read', async () => {
        // Write a doc whose _v is not declared in the manifest's versions
        // array. STRICT off so it is stored as-is (validateAgainstHead would
        // otherwise re-stamp with the head label).
        Fylo.STRICT = undefined
        const id = await fylo.putData(COLLECTION, {
            id: 4,
            title: 'From the future',
            body: 'tomorrow',
            slug: 'future',
            _v: 'v99'
        })

        await expect(fylo.getLatest(COLLECTION, id)).rejects.toThrow(/unknown version/)
    })

    test('FYLO schemas reject arrays of objects even when CHEX supports them', async () => {
        const schemaRoot = await mkdtemp(path.join(os.tmpdir(), 'fylo-array-object-schema-'))
        const collection = 'object-list'
        const collectionRoot = path.join(schemaRoot, collection)
        await mkdir(path.join(collectionRoot, 'history'), { recursive: true })
        await writeFile(
            path.join(collectionRoot, 'manifest.json'),
            JSON.stringify({ current: 'v1', versions: [{ v: 'v1' }] })
        )
        await writeFile(
            path.join(collectionRoot, 'history', 'v1.schema.json'),
            JSON.stringify({ items: [{ name: '^[A-Za-z]+$' }] })
        )

        try {
            await expect(
                validateAgainstHead(
                    collection,
                    { items: [{ name: 'Ada' }] },
                    { schemaDir: schemaRoot }
                )
            ).rejects.toThrow(/does not support arrays of objects/)
        } finally {
            _resetCaches()
            await rm(schemaRoot, { recursive: true, force: true })
        }
    })
})
