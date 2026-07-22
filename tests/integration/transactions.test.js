import { afterAll, describe, expect, test } from 'bun:test'
import { mkdir, open, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-transactions-')
const CRASH_WORKER = path.join(import.meta.dir, 'transaction-crash.worker.js')

async function values(fylo, collection, field) {
    const result = []
    for await (const entry of fylo[collection].find().collect()) {
        const document = Object.values(entry)[0]
        result.push(document[field])
    }
    return result.sort()
}

describe('collection transactions', () => {
    afterAll(async () => {
        await rm(root, { recursive: true, force: true })
    })

    test('an UPDATE statement rolls back every matched document when one write fails', async () => {
        const collection = `atomic-update-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        await fylo[collection].put({ state: 'before', ordinal: 1 })
        await fylo[collection].put({ state: 'before', ordinal: 2 })
        await fylo[collection].put({ state: 'before', ordinal: 3 })

        const updateDocument = fylo.engine.updateDocument.bind(fylo.engine)
        let attempts = 0
        fylo.engine.updateDocument = async (...args) => {
            attempts++
            if (attempts === 2) throw new Error('injected second-document failure')
            return await updateDocument(...args)
        }

        await expect(
            fylo._sql(`UPDATE ${collection} SET state = 'after' WHERE state = 'before'`)
        ).rejects.toThrow('injected second-document failure')
        expect(await values(fylo, collection, 'state')).toEqual(['before', 'before', 'before'])
    })

    test('a reader retries when the collection generation changes during its scan', async () => {
        const collection = `snapshot-read-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        await fylo[collection].put({ state: 'before', ordinal: 1 })
        await fylo[collection].put({ state: 'before', ordinal: 2 })

        const readStoredRecord = fylo.engine.readStoredRecord.bind(fylo.engine)
        let firstRead
        const firstReadStarted = new Promise((resolve) => {
            firstRead = resolve
        })
        let releaseRead
        const readMayContinue = new Promise((resolve) => {
            releaseRead = resolve
        })
        let reads = 0
        fylo.engine.readStoredRecord = async (...args) => {
            const result = await readStoredRecord(...args)
            reads++
            if (reads === 1) {
                firstRead()
                await readMayContinue
            }
            return result
        }

        const scan = values(fylo, collection, 'state')
        await firstReadStarted
        await fylo._sql(`UPDATE ${collection} SET state = 'after' WHERE state = 'before'`)
        releaseRead()

        expect(await scan).toEqual(['after', 'after'])
    })

    test('startup read rolls back a transaction interrupted by SIGKILL', async () => {
        const collection = `crash-update-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        await fylo[collection].put({ state: 'before', ordinal: 1 })
        await fylo[collection].put({ state: 'before', ordinal: 2 })

        const worker = Bun.spawn(['bun', CRASH_WORKER, root, collection], {
            stdout: 'pipe',
            stderr: 'pipe'
        })
        await worker.exited

        const recovered = new Fylo(root, { versioning: { autoCommit: false } })
        expect(await values(recovered, collection, 'state')).toEqual(['before', 'before'])
        expect(await recovered.engine.transactions.state(collection)).toMatchObject({
            state: 'stable'
        })
    }, 10_000)

    test('bulk transactions append bounded capture segments without rewriting the manifest', async () => {
        const collection = `bulk-journal-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        for (let ordinal = 0; ordinal < 250; ordinal += 1) {
            await fylo[collection].put({ state: 'before', ordinal })
        }
        const writeManifest = fylo.engine.transactions.writeManifest.bind(fylo.engine.transactions)
        let manifestWrites = 0
        fylo.engine.transactions.writeManifest = async (...args) => {
            manifestWrites += 1
            return await writeManifest(...args)
        }

        await fylo._sql(`UPDATE ${collection} SET state = 'after' WHERE state = 'before'`)
        expect(manifestWrites).toBe(2)
        expect(await values(fylo, collection, 'state')).toHaveLength(250)
        expect((await fylo.recoveryStatus(collection)).state).toBe('stable')
    }, 30_000)

    test('restart rolls back a realistically sized segmented journal', async () => {
        const collection = `bulk-crash-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        for (let ordinal = 0; ordinal < 160; ordinal += 1) {
            await fylo[collection].put({ state: 'before', ordinal })
        }
        const worker = Bun.spawn(['bun', CRASH_WORKER, root, collection, '80'], {
            stdout: 'pipe',
            stderr: 'pipe'
        })
        await worker.exited

        const recovered = new Fylo(root, { versioning: { autoCommit: false } })
        expect(await values(recovered, collection, 'state')).toHaveLength(160)
        expect(new Set(await values(recovered, collection, 'state'))).toEqual(new Set(['before']))
    }, 30_000)

    test('rollback emits structured status and remains operator-inspectable', async () => {
        const collection = `rollback-status-${Date.now()}`
        const events = []
        const fylo = new Fylo(root, {
            versioning: { autoCommit: false },
            onEvent: (event) => events.push(event)
        })
        await fylo[collection].create()
        await fylo[collection].put({ state: 'before' })
        await expect(
            fylo.engine.atomic(collection, 'operator-test', async () => {
                throw new Error('operator-visible failure')
            })
        ).rejects.toThrow('operator-visible failure')

        expect(events.map((event) => event.type)).toEqual([
            'transaction.rollback.started',
            'index.rebuilt',
            'transaction.rollback.succeeded'
        ])
        expect(await fylo.recoveryStatus(collection)).toMatchObject({
            state: 'stable',
            activity: { status: 'idle', lastAction: 'rollback', operation: 'operator-test' }
        })
    })

    test('a DELETE statement rolls back earlier deletes when a later delete fails', async () => {
        const collection = `atomic-delete-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        await fylo[collection].put({ state: 'keep', ordinal: 1 })
        await fylo[collection].put({ state: 'keep', ordinal: 2 })

        const deleteDocument = fylo.engine.deleteDocument.bind(fylo.engine)
        let attempts = 0
        fylo.engine.deleteDocument = async (...args) => {
            attempts++
            if (attempts === 2) throw new Error('injected delete failure')
            return await deleteDocument(...args)
        }

        await expect(fylo._sql(`DELETE FROM ${collection} WHERE state = 'keep'`)).rejects.toThrow(
            'injected delete failure'
        )
        expect(await values(fylo, collection, 'state')).toEqual(['keep', 'keep'])
    })

    test('rollback restores document xattrs exactly', async () => {
        const collection = `atomic-meta-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        const id = await fylo[collection].put({ title: 'metadata' })
        await fylo[collection].put(id).metadata({ owner: 'before' })

        await expect(
            fylo.engine.atomic(collection, 'metadata-test', async () => {
                await fylo.engine.setDocMetaRecord(collection, id, { owner: 'after', extra: true })
                throw new Error('abort metadata transaction')
            })
        ).rejects.toThrow('abort metadata transaction')

        expect(await fylo[collection].get(id).metadata()).toMatchObject({ owner: 'before' })
        expect(await fylo[collection].get(id).metadata()).not.toHaveProperty('extra')
    })

    test('a missing active manifest fails closed instead of exposing uncertain data', async () => {
        const collection = `corrupt-transaction-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        const id = await fylo[collection].put({ state: 'known' })
        const transactionRoot = path.join(root, '.fylo-transactions', '.collections', collection)
        await mkdir(transactionRoot, { recursive: true })
        await writeFile(
            path.join(transactionRoot, 'state.json'),
            JSON.stringify({
                format: 'fylo.collection-generation.v1',
                generation: 3,
                state: 'writing',
                transactionId: 'missing'
            })
        )

        const reopened = new Fylo(root, { versioning: { autoCommit: false } })
        await expect(reopened.engine.getLatest(collection, id)).rejects.toThrow(
            'manifest is missing or corrupt'
        )
    })

    test('recovery rejects traversal paths without changing an outside sentinel', async () => {
        const collection = `traversal-transaction-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        const transactionRoot = path.join(root, '.fylo-transactions', '.collections', collection)
        const transactionId = '0190f5b0-1111-7111-8111-111111111111'
        const sentinel = path.join(root, 'journal-sentinel.txt')
        await writeFile(sentinel, 'untouched')
        await mkdir(path.join(transactionRoot, transactionId), { recursive: true })
        await writeFile(
            path.join(transactionRoot, 'state.json'),
            JSON.stringify({
                format: 'fylo.collection-generation.v1',
                generation: 1,
                state: 'writing',
                transactionId
            })
        )
        await writeFile(
            path.join(transactionRoot, transactionId, 'transaction.json'),
            JSON.stringify({
                format: 'fylo.collection-transaction.v1',
                id: transactionId,
                collection,
                operation: 'malicious-recovery',
                phase: 'active',
                generationBefore: 0,
                eventOffset: 0,
                captures: [{ path: '../../../journal-sentinel.txt', present: false }]
            })
        )

        await expect(fylo.engine.recoverCollection(collection)).rejects.toThrow(
            'manifest is missing or corrupt'
        )
        expect(await readFile(sentinel, 'utf8')).toBe('untouched')
    })

    test('recovery rejects symlinked capture paths without changing an outside sentinel', async () => {
        const collection = `symlink-transaction-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        const collectionRoot = path.join(root, '.collections', collection)
        const outside = path.join(root, `outside-${Date.now()}`)
        const sentinel = path.join(outside, 'sentinel.txt')
        await mkdir(outside, { recursive: true })
        await writeFile(sentinel, 'untouched')
        await symlink(
            outside,
            path.join(collectionRoot, 'escape'),
            process.platform === 'win32' ? 'junction' : 'dir'
        )

        const journalRoot = path.join(root, '.fylo-transactions', '.collections', collection)
        const transactionId = '0190f5b0-2222-7222-8222-222222222222'
        await mkdir(path.join(journalRoot, transactionId), { recursive: true })
        await writeFile(
            path.join(journalRoot, 'state.json'),
            JSON.stringify({
                format: 'fylo.collection-generation.v1',
                generation: 1,
                state: 'writing',
                transactionId
            })
        )
        await writeFile(
            path.join(journalRoot, transactionId, 'transaction.json'),
            JSON.stringify({
                format: 'fylo.collection-transaction.v1',
                id: transactionId,
                collection,
                operation: 'malicious-recovery',
                phase: 'active',
                generationBefore: 0,
                eventOffset: 0,
                captures: [{ path: 'escape/sentinel.txt', present: false }]
            })
        )

        await expect(fylo.engine.recoverCollection(collection)).rejects.toThrow(
            'manifest is missing or corrupt'
        )
        expect(await readFile(sentinel, 'utf8')).toBe('untouched')
    })

    test('state metadata is rejected before reading beyond its byte budget', async () => {
        const collection = `oversized-state-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        const statePath = path.join(
            root,
            '.fylo-transactions',
            '.collections',
            collection,
            'state.json'
        )
        await mkdir(path.dirname(statePath), { recursive: true })
        const handle = await open(statePath, 'w')
        await handle.truncate(16 * 1024 + 1)
        await handle.close()
        await expect(fylo.engine.transactions.state(collection)).rejects.toThrow(
            'Transaction metadata is corrupt'
        )
    })

    test('manifest metadata is rejected before reading beyond its byte budget', async () => {
        const collection = `oversized-manifest-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        const journalRoot = path.join(root, '.fylo-transactions', '.collections', collection)
        const transactionId = '0190f5b0-3333-7333-8333-333333333333'
        await mkdir(path.join(journalRoot, transactionId), { recursive: true })
        await writeFile(
            path.join(journalRoot, 'state.json'),
            JSON.stringify({
                format: 'fylo.collection-generation.v1',
                generation: 1,
                state: 'writing',
                transactionId
            })
        )
        const handle = await open(path.join(journalRoot, transactionId, 'transaction.json'), 'w')
        await handle.truncate(16 * 1024 * 1024 + 1)
        await handle.close()

        await expect(fylo.engine.recoverCollection(collection)).rejects.toThrow(
            'manifest is missing or corrupt'
        )
    })

    test('manifest xattrs above the aggregate budget fail closed', async () => {
        const collection = `oversized-xattrs-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        const sentinel = path.join(root, `xattr-sentinel-${Date.now()}.txt`)
        await writeFile(sentinel, 'untouched')
        const journalRoot = path.join(root, '.fylo-transactions', '.collections', collection)
        const transactionId = '0190f5b0-4444-7444-8444-444444444444'
        const before = path.join(journalRoot, transactionId, 'before')
        await mkdir(before, { recursive: true })
        await writeFile(path.join(before, '000000.bin'), 'before')
        await writeFile(
            path.join(journalRoot, 'state.json'),
            JSON.stringify({
                format: 'fylo.collection-generation.v1',
                generation: 1,
                state: 'writing',
                transactionId
            })
        )
        const oversized = Buffer.alloc(1024 * 1024 + 1).toString('base64')
        await writeFile(
            path.join(journalRoot, transactionId, 'transaction.json'),
            JSON.stringify({
                format: 'fylo.collection-transaction.v1',
                id: transactionId,
                collection,
                operation: 'malicious-recovery',
                phase: 'active',
                generationBefore: 0,
                eventOffset: 0,
                captures: [
                    {
                        path: 'docs/target.json',
                        present: true,
                        backup: 'before/000000.bin',
                        mode: 0o600,
                        mtimeMs: Date.now(),
                        xattrs: [{ name: 'user.large', value: oversized }]
                    }
                ]
            })
        )

        await expect(fylo.engine.recoverCollection(collection)).rejects.toThrow(
            'manifest is missing or corrupt'
        )
        expect(await readFile(sentinel, 'utf8')).toBe('untouched')
    })

    test('recovery rolls forward after the durable commit marker', async () => {
        const collection = `commit-recovery-${Date.now()}`
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo[collection].create()
        const originalWriteState = fylo.engine.transactions.writeState.bind(
            fylo.engine.transactions
        )
        let injected = false
        fylo.engine.transactions.writeState = async (name, state) => {
            if (!injected && state.state === 'stable') {
                injected = true
                throw new Error('injected generation publish failure')
            }
            await originalWriteState(name, state)
        }

        const id = '4UUB1111111'
        await expect(
            Promise.resolve(fylo[collection].put(id, { state: 'committed' }))
        ).rejects.toThrow('generation publish failure')
        fylo.engine.transactions.writeState = originalWriteState

        expect((await fylo[collection].get(id).once())[id]).toEqual({
            state: 'committed'
        })
        expect(await fylo.engine.transactions.state(collection)).toMatchObject({ state: 'stable' })
    })
})
