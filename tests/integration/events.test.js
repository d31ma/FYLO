import { afterAll, describe, expect, test } from 'bun:test'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { Cipher } from '../../src/security/cipher.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-events-')

describe('FYLO onEvent hook', () => {
    afterAll(async () => {
        await rm(root, { recursive: true, force: true })
    })

    test('emits import.blocked when SSRF guard rejects a private address', async () => {
        /** @type {import('../../src/observability/events.js').FyloEvent[]} */
        const events = []
        const fylo = new Fylo(root, { onEvent: (e) => events.push(e) })
        const collection = `evt-import-${Date.now()}`
        await fylo[collection].create()
        await expect(fylo[collection].import(new URL('http://127.0.0.1/x.json'))).rejects.toThrow(
            'private address'
        )
        const blocked = events.find((e) => e.type === 'import.blocked')
        expect(blocked).toBeDefined()
        if (blocked && blocked.type === 'import.blocked') {
            expect(blocked.reason).toBe('private-network')
            expect(blocked.url).toBe('http://127.0.0.1/x.json')
        }
    })

    test('emits import.blocked with reason=protocol for unsupported scheme', async () => {
        /** @type {import('../../src/observability/events.js').FyloEvent[]} */
        const events = []
        const fylo = new Fylo(root, { onEvent: (e) => events.push(e) })
        const collection = `evt-proto-${Date.now()}`
        await fylo[collection].create()
        await expect(fylo[collection].import(new URL('ftp://example.com/x.json'))).rejects.toThrow(
            'protocol is not allowed'
        )
        const blocked = events.find((e) => e.type === 'import.blocked')
        expect(blocked).toBeDefined()
        if (blocked && blocked.type === 'import.blocked') {
            expect(blocked.reason).toBe('protocol')
        }
    })

    test('emits sync.failed when a fire-and-forget sync hook rejects', async () => {
        /** @type {import('../../src/observability/events.js').FyloEvent[]} */
        const events = []
        const syncRoot = await createTestRoot('fylo-sync-fail-')
        try {
            const fylo = new Fylo(syncRoot, {
                syncMode: 'fire-and-forget',
                sync: {
                    onWrite: () => {
                        throw new Error('replica unreachable')
                    }
                },
                onEvent: (e) => events.push(e)
            })
            const collection = `evt-sync-${Date.now()}`
            await fylo[collection].create()
            await fylo[collection].put({ title: 'will sync' })
            // Fire-and-forget: the hook rejects on a later turn, so let it settle.
            await new Promise((resolve) => setTimeout(resolve, 50))
            const failed = events.find((e) => e.type === 'sync.failed')
            expect(failed).toBeDefined()
            if (failed && failed.type === 'sync.failed') {
                expect(failed.collection).toBe(collection)
                expect(failed.operation).toBe('put')
                expect(failed.detail).toContain('replica unreachable')
            }
        } finally {
            await rm(syncRoot, { recursive: true, force: true })
        }
    })

    test('emits cipher.configured when schema-driven config flips Cipher state', async () => {
        const previousSchema = process.env.FYLO_SCHEMA
        const previousEncryptionKey = process.env.FYLO_ENCRYPTION_KEY
        const previousSalt = process.env.FYLO_CIPHER_SALT
        const schemaRoot = await mkdtemp(path.join(os.tmpdir(), 'fylo-events-schema-'))
        const collection = `evt-cipher-${Date.now()}`
        Cipher.reset()
        Fylo.loadedEncryption.delete(collection)
        await Bun.write(
            path.join(schemaRoot, collection, 'history', 'v1.schema.json'),
            JSON.stringify({ $encrypted: ['secret'] })
        )
        await Bun.write(
            path.join(schemaRoot, collection, 'manifest.json'),
            JSON.stringify({
                current: 'v1',
                versions: [{ v: 'v1', addedAt: '2026-04-01T00:00:00Z' }]
            })
        )
        process.env.FYLO_SCHEMA = schemaRoot
        process.env.FYLO_ENCRYPTION_KEY = 'k'.repeat(48)
        process.env.FYLO_CIPHER_SALT = 'deadbeef'.repeat(8)
        /** @type {import('../../src/observability/events.js').FyloEvent[]} */
        const events = []
        try {
            const fylo = new Fylo(root, { onEvent: (e) => events.push(e) })
            await fylo[collection].create()
            await fylo[collection].put({ secret: 'shh' })
            const cipherEvent = events.find((e) => e.type === 'cipher.configured')
            expect(cipherEvent).toBeDefined()
            if (cipherEvent && cipherEvent.type === 'cipher.configured') {
                expect(cipherEvent.collection).toBe(collection)
            }
        } finally {
            if (previousSchema === undefined) delete process.env.FYLO_SCHEMA
            else process.env.FYLO_SCHEMA = previousSchema
            if (previousEncryptionKey === undefined) delete process.env.FYLO_ENCRYPTION_KEY
            else process.env.FYLO_ENCRYPTION_KEY = previousEncryptionKey
            if (previousSalt === undefined) delete process.env.FYLO_CIPHER_SALT
            else process.env.FYLO_CIPHER_SALT = previousSalt
            Cipher.reset()
            await rm(schemaRoot, { recursive: true, force: true })
        }
    })

    test('emits index.rebuilt at the end of rebuildCollection', async () => {
        /** @type {import('../../src/observability/events.js').FyloEvent[]} */
        const events = []
        const fylo = new Fylo(root, { onEvent: (e) => events.push(e) })
        const collection = `evt-rebuild-${Date.now()}`
        await fylo[collection].create()
        await fylo[collection].put({ title: 'one' })
        await fylo[collection].put({ title: 'two' })
        await fylo[collection].rebuild()
        const rebuilt = events.find((e) => e.type === 'index.rebuilt')
        expect(rebuilt).toBeDefined()
        if (rebuilt && rebuilt.type === 'index.rebuilt') {
            expect(rebuilt.collection).toBe(collection)
            expect(rebuilt.docsScanned).toBe(2)
            expect(rebuilt.indexedDocs).toBe(2)
            expect(rebuilt.worm).toBe(false)
        }
    })

    test('emits lock.takeover when a stale collection write-lock is reclaimed', async () => {
        const lockRoot = await createTestRoot('fylo-events-lock-')
        const collection = `evt-takeover-${Date.now()}`
        try {
            const fylo = new Fylo(lockRoot)
            await fylo[collection].create()
            const lockPath = path.join(
                lockRoot,
                '.collections',
                collection,
                'locks',
                'collection.lock'
            )
            // Plant a lock file older than the collection-write TTL (5 min) so
            // the next acquirer treats it as stale and takes it over.
            await mkdir(path.dirname(lockPath), { recursive: true })
            await writeFile(
                lockPath,
                JSON.stringify({ owner: 'dead-owner', ts: Date.now() - 600_000 })
            )
            /** @type {import('../../src/observability/events.js').FyloEvent[]} */
            const events = []
            const observer = new Fylo(lockRoot, { onEvent: (e) => events.push(e) })
            await observer[collection].put({ title: 'after-takeover' })
            const takeover = events.find((e) => e.type === 'lock.takeover')
            expect(takeover).toBeDefined()
            if (takeover && takeover.type === 'lock.takeover') {
                expect(takeover.lockPath).toBe(lockPath)
                expect(takeover.previousOwner).toBe('dead-owner')
            }
        } finally {
            await rm(lockRoot, { recursive: true, force: true })
        }
    })

    test('a throwing onEvent handler does not break the underlying operation', async () => {
        const fylo = new Fylo(root, {
            onEvent: () => {
                throw new Error('handler boom')
            }
        })
        const collection = `evt-throw-${Date.now()}`
        await fylo[collection].create()
        await fylo[collection].put({ title: 'one' })
        const result = await fylo[collection].rebuild()
        expect(result.indexedDocs).toBe(1)
    })

    test('a rejecting async onEvent handler does not break the underlying operation', async () => {
        let unhandled = false
        const onUnhandled = () => {
            unhandled = true
        }
        process.on('unhandledRejection', onUnhandled)
        try {
            const fylo = new Fylo(root, {
                onEvent: async () => {
                    throw new Error('handler async boom')
                }
            })
            const collection = `evt-reject-${Date.now()}`
            await fylo[collection].create()
            await fylo[collection].put({ title: 'one' })
            const result = await fylo[collection].rebuild()
            expect(result.indexedDocs).toBe(1)
            await new Promise((r) => setTimeout(r, 50))
            expect(unhandled).toBe(false)
        } finally {
            process.off('unhandledRejection', onUnhandled)
        }
    })
})
