import { test, expect, describe, beforeAll, afterAll, mock } from 'bun:test'
import { readdir, rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'
import { CipherMock } from '../mocks/cipher.js'
const COLLECTION = 'encrypted-test'
const root = await createTestRoot('fylo-encryption-')
const fylo = new Fylo({ root })
mock.module('../../src/security/cipher', () => ({ Cipher: CipherMock }))

async function readTree(target) {
    let text = ''
    let entries = []
    try {
        entries = await readdir(target, { withFileTypes: true })
    } catch {
        return text
    }
    for (const entry of entries) {
        const child = path.join(target, entry.name)
        if (entry.isDirectory()) text += await readTree(child)
        else text += `${child}\n${await Bun.file(child).text()}\n`
    }
    return text
}
beforeAll(async () => {
    await fylo.createCollection(COLLECTION)
    await CipherMock.configure('test-secret-key')
    CipherMock.registerFields(COLLECTION, ['email', 'ssn', 'address'])
})
afterAll(async () => {
    CipherMock.reset()
    mock.restore()
    await fylo.dropCollection(COLLECTION)
    await rm(root, { recursive: true, force: true })
})
describe('Encryption', () => {
    let docId
    test('PUT encrypted document', async () => {
        docId = await fylo.putData(COLLECTION, {
            name: 'Alice',
            email: 'alice@example.com',
            ssn: '123-45-6789',
            age: 30
        })
        expect(docId).toBeDefined()
    })
    test('GET decrypts fields transparently', async () => {
        const result = await fylo.getDoc(COLLECTION, docId).once()
        const doc = Object.values(result)[0]
        expect(doc.name).toBe('Alice')
        expect(doc.email).toBe('alice@example.com')
        expect(doc.ssn).toBe('123-45-6789')
        expect(doc.age).toBe(30)
    })
    test('encrypted values stored in the doc file are not plaintext', async () => {
        const raw = await Bun.file(
            path.join(root, '.collections', COLLECTION, 'docs', docId.slice(0, 2), `${docId}.json`)
        ).text()
        expect(raw).not.toContain('alice@example.com')
        expect(raw).not.toContain('123-45-6789')
    })
    test('encrypted values are not plaintext in indexes or event journals', async () => {
        const index = await readTree(path.join(root, '.collections', COLLECTION, 'index'))
        const events = await Bun.file(
            path.join(root, '.collections', COLLECTION, 'events', `${COLLECTION}.ndjson`)
        ).text()

        expect(index).not.toContain('alice@example.com')
        expect(index).not.toContain('123-45-6789')
        expect(events).not.toContain('alice@example.com')
        expect(events).not.toContain('123-45-6789')
    })
    test('$eq query works on encrypted field', async () => {
        let found = false
        for await (const data of fylo
            .findDocs(COLLECTION, {
                $ops: [{ email: { $eq: 'alice@example.com' } }]
            })
            .collect()) {
            if (typeof data === 'object') {
                const doc = Object.values(data)[0]
                expect(doc.email).toBe('alice@example.com')
                found = true
            }
        }
        expect(found).toBe(true)
    })
    test('$ne throws on encrypted field', async () => {
        try {
            const iter = fylo
                .findDocs(COLLECTION, {
                    $ops: [{ email: { $ne: 'bob@example.com' } }]
                })
                .collect()
            await iter.next()
            expect(true).toBe(false)
        } catch (e) {
            expect(e.message).toContain('not supported on encrypted field')
        }
    })
    test('$gt throws on encrypted field', async () => {
        try {
            const iter = fylo
                .findDocs(COLLECTION, {
                    $ops: [{ ssn: { $gt: 0 } }]
                })
                .collect()
            await iter.next()
            expect(true).toBe(false)
        } catch (e) {
            expect(e.message).toContain('not supported on encrypted field')
        }
    })
    test('$like throws on encrypted field', async () => {
        try {
            const iter = fylo
                .findDocs(COLLECTION, {
                    $ops: [{ email: { $like: '%@example.com' } }]
                })
                .collect()
            await iter.next()
            expect(true).toBe(false)
        } catch (e) {
            expect(e.message).toContain('not supported on encrypted field')
        }
    })
    test('non-encrypted fields remain queryable with all operators', async () => {
        let found = false
        for await (const data of fylo
            .findDocs(COLLECTION, {
                $ops: [{ name: { $eq: 'Alice' } }]
            })
            .collect()) {
            if (typeof data === 'object') {
                const doc = Object.values(data)[0]
                expect(doc.name).toBe('Alice')
                found = true
            }
        }
        expect(found).toBe(true)
    })
    test('nested encrypted field (address.city)', async () => {
        const id = await fylo.putData(COLLECTION, {
            name: 'Bob',
            email: 'bob@example.com',
            ssn: '987-65-4321',
            age: 25,
            address: { city: 'Toronto', zip: 'M5V 2T6' }
        })
        const result = await fylo.getDoc(COLLECTION, id).once()
        const doc = Object.values(result)[0]
        expect(doc.address.city).toBe('Toronto')
        expect(doc.address.zip).toBe('M5V 2T6')
    })
    test('UPDATE preserves encryption', async () => {
        await fylo.patchDoc(COLLECTION, {
            [docId]: { email: 'alice-new@example.com' }
        })
        let found = false
        for await (const data of fylo
            .findDocs(COLLECTION, {
                $ops: [{ email: { $eq: 'alice-new@example.com' } }]
            })
            .collect()) {
            if (typeof data === 'object') {
                const doc = Object.values(data)[0]
                expect(doc.email).toBe('alice-new@example.com')
                found = true
            }
        }
        expect(found).toBe(true)
    })
    test('schema encrypted fields fail closed when FYLO_ENCRYPTION_KEY is absent', async () => {
        const previousSchemaDir = process.env.FYLO_SCHEMA_DIR
        const previousEncryptionKey = process.env.FYLO_ENCRYPTION_KEY
        const schemaRoot = await createTestRoot('fylo-schema-')
        const collection = `fail-closed-${Date.now()}`

        CipherMock.reset()
        await Bun.write(
            path.join(schemaRoot, collection, 'history', 'v1.json'),
            JSON.stringify({ $encrypted: ['secret'] })
        )
        await Bun.write(
            path.join(schemaRoot, collection, 'manifest.json'),
            JSON.stringify({
                current: 'v1',
                versions: [{ v: 'v1', addedAt: '2026-04-01T00:00:00Z' }]
            })
        )
        process.env.FYLO_SCHEMA_DIR = schemaRoot
        delete process.env.FYLO_ENCRYPTION_KEY

        try {
            const guardedFylo = new Fylo({ root })
            await guardedFylo.createCollection(collection)
            await expect(
                guardedFylo.putData(collection, { secret: 'do not store' })
            ).rejects.toThrow('FYLO_ENCRYPTION_KEY')
        } finally {
            if (previousSchemaDir === undefined) delete process.env.FYLO_SCHEMA_DIR
            else process.env.FYLO_SCHEMA_DIR = previousSchemaDir
            if (previousEncryptionKey === undefined) delete process.env.FYLO_ENCRYPTION_KEY
            else process.env.FYLO_ENCRYPTION_KEY = previousEncryptionKey
            await rm(schemaRoot, { recursive: true, force: true })
        }
    })
})
