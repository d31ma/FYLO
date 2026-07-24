import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtemp, mkdir, open, readFile, rename, rm, symlink, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { FyloS3Backup } from '../../src/replication/s3-backup.js'
import { createTestRoot } from '../helpers/root.js'

// A stub Bun.S3Client backed by an in-memory bucket, so the whole engine ->
// FyloS3Backup path runs deterministically without real credentials.
const originalS3Client = Bun.S3Client
/** @type {Map<string, Buffer>} object key -> bytes */
const objects = new Map()
/** @type {string[]} */
const writes = []

class FakeS3Client {
    /** @param {Record<string, any>} options */
    constructor(options) {
        this.options = options
    }
    /** @param {string} key @param {string | Uint8Array} body */
    async write(key, body) {
        writes.push(key)
        objects.set(key, Buffer.isBuffer(body) ? body : Buffer.from(body))
    }
    /** @param {string} key */
    async delete(key) {
        objects.delete(key)
    }
    /** @param {{ prefix?: string, startAfter?: string }} [opts] */
    async list({ prefix = '', startAfter } = {}) {
        const keys = [...objects.keys()].filter((k) => k.startsWith(prefix)).sort()
        const page = startAfter ? keys.filter((k) => k > startAfter) : keys
        return {
            contents: page.map((k) => ({
                key: k,
                size: /** @type {Buffer} */ (objects.get(k)).length
            })),
            isTruncated: false
        }
    }
    /** @param {string} key */
    file(key) {
        return {
            stream: () => {
                const value = objects.get(key)
                if (!value) throw new Error(`Missing S3 object: ${key}`)
                return new Blob([value]).stream()
            }
        }
    }
}

function xattrManifestFor(key) {
    return [...objects.entries()].find(([candidate, bytes]) => {
        if (!candidate.includes('.fylo-backup/xattrs/') || !candidate.endsWith('.json')) {
            return false
        }
        try {
            return JSON.parse(bytes.toString()).dataKey === key
        } catch {
            return false
        }
    })?.[1]
}

/** @type {string} */
let root
/** @type {Fylo} */
let fylo

beforeAll(async () => {
    Bun.S3Client = /** @type {any} */ (FakeS3Client)
    root = await createTestRoot('fylo-s3-backup-')
    fylo = new Fylo(root, {
        syncMode: 'await-sync', // mirror completes before the write resolves
        versioning: { autoCommit: false },
        sync: { s3: { bucket: 'fylo-backup', prefix: 'tenant-a' } }
    })
    await fylo.ready()
    await fylo.users.create()
})

afterAll(async () => {
    await fylo.close()
    Bun.S3Client = originalS3Client
    await rm(root, { recursive: true, force: true })
})

describe('built-in whole-root S3 backup', () => {
    test('mirror on write pushes the doc, its index, and the catalog', async () => {
        const id = await fylo.users.put({ name: 'Ada', role: 'admin' })
        const keys = [...objects.keys()]
        expect(
            keys.some((k) => k.includes('.collections/users/docs/') && k.includes(String(id)))
        ).toBe(true)
        expect(
            keys.some(
                (k) =>
                    k.endsWith('.collections/users/index/keys.snapshot') ||
                    k.endsWith('.collections/users/index/keys.wal')
            )
        ).toBe(true)
        expect(keys.some((k) => k.endsWith('.fylo-catalog/collections/users.json'))).toBe(true)
    })

    test('delete drops the doc object and re-mirrors the index', async () => {
        const id = await fylo.users.put({ name: 'Grace' })
        const docKey = [...objects.keys()].find(
            (k) => k.includes('.collections/users/docs/') && k.includes(String(id))
        )
        expect(docKey).toBeTruthy()

        await fylo.users.delete(id)
        expect(objects.has(/** @type {string} */ (docKey))).toBe(false)
        // The index sidecar is still present (re-mirrored on delete).
        expect([...objects.keys()].some((k) => k.includes('/.collections/users/index/'))).toBe(true)
    })

    test('reconcile makes S3 match the whole root: uploads all, prunes orphans', async () => {
        // A stale object with no local counterpart, and a raw bucket file.
        const backup = fylo.engine.backup
        const orphan = `${backup.prefix}/ghost/orphan.txt`
        const unrelated = 'another-tenant/keep.txt'
        objects.set(orphan, Buffer.from('stale'))
        objects.set(unrelated, Buffer.from('belongs to someone else'))
        await fylo.assets.create({ kind: 'file' })
        await fylo.assets.put(new File(['hello'], 'hi.txt'), { key: '/hi.txt' })

        await fylo.reconcile()

        expect(objects.has(orphan)).toBe(false)
        expect(objects.get(unrelated)?.toString()).toBe('belongs to someone else')
        expect([...objects.keys()].some((k) => k.includes('.buckets/assets/'))).toBe(true)
        expect(
            [...objects.keys()].some((k) => k.endsWith('.fylo-catalog/collections/assets.json'))
        ).toBe(true)
        expect([...objects.keys()].some((k) => k.includes('.fylo-transactions/'))).toBe(false)
    })

    test('requires an explicit prefix unless bucket-root deletion is deliberately enabled', () => {
        expect(() => new FyloS3Backup({ bucket: 'shared' }, root)).toThrow(
            'sync.s3.prefix must be non-empty'
        )
        expect(
            () => new FyloS3Backup({ bucket: 'dedicated', allowBucketRoot: true }, root)
        ).not.toThrow()
        expect(
            () => new FyloS3Backup({ bucket: 'shared', prefix: 'tenant/../escape' }, root)
        ).toThrow('normalized S3 key prefix')
    })

    test('never uploads outside-root bytes or xattrs after a parent-directory swap', async () => {
        if (process.platform === 'win32') return
        const backup = fylo.engine.backup
        const directory = path.join(root, 'swap-parent')
        const parked = path.join(root, 'swap-parent-parked')
        const outside = await mkdtemp(path.join(os.tmpdir(), 'fylo-s3-outside-'))
        const target = path.join(directory, 'payload.txt')
        await mkdir(directory)
        await writeFile(target, 'safe inside bytes')
        await writeFile(path.join(outside, 'payload.txt'), 'secret outside bytes')

        const client = backup.client
        const originalList = client.list
        let swapped = false
        client.list = async (...args) => {
            if (!swapped) {
                swapped = true
                await rename(directory, parked)
                await symlink(outside, directory, 'dir')
            }
            return originalList.apply(client, args)
        }

        try {
            await fylo.reconcile()
            const uploaded = objects.get(backup.key(target))
            expect(uploaded?.toString()).not.toBe('secret outside bytes')
            const manifest = xattrManifestFor(backup.key(target))
            if (manifest) expect(manifest.toString()).not.toContain('secret outside')
        } finally {
            client.list = originalList
            await rm(directory, { force: true })
            await rename(parked, directory)
            await rm(directory, { recursive: true, force: true })
            await rm(outside, { recursive: true, force: true })
        }
    })

    test('reconcile excludes durable scratch siblings and symbolic links', async () => {
        const uuid = '019f694e-b56b-7000-abc8-c6c912344a03'
        const scratchKey = `catalog.json.${uuid}.tmp`
        const scratchPath = path.join(root, scratchKey)
        const linkKey = 'catalog-link.json'
        const linkPath = path.join(root, linkKey)
        const backup = fylo.engine.backup
        const scratchObjectKey = backup.key(scratchPath)
        const linkObjectKey = backup.key(linkPath)

        await writeFile(scratchPath, 'not committed')
        await symlink(path.join(root, '.fylo-catalog', 'collections', 'users.json'), linkPath)
        objects.set(scratchObjectKey, Buffer.from('stale scratch'))
        objects.set(backup.manifestKey(scratchObjectKey), Buffer.from('{}'))
        objects.set(linkObjectKey, Buffer.from('stale link'))
        objects.set(backup.manifestKey(linkObjectKey), Buffer.from('{}'))
        writes.length = 0

        try {
            await fylo.reconcile()

            expect(writes).not.toContain(scratchObjectKey)
            expect(writes).not.toContain(linkObjectKey)
            expect(objects.has(scratchObjectKey)).toBe(false)
            expect(objects.has(backup.manifestKey(scratchObjectKey))).toBe(false)
            expect(objects.has(linkObjectKey)).toBe(false)
            expect(objects.has(backup.manifestKey(linkObjectKey))).toBe(false)
        } finally {
            await rm(scratchPath, { force: true })
            await rm(linkPath, { force: true })
        }
    })

    test('reconcile tolerates a file disappearing after the root scan', async () => {
        const key = 'vanishing.txt'
        const target = path.join(root, key)
        const backup = fylo.engine.backup
        const objectKey = backup.key(target)
        const client = backup.client
        const originalList = client.list
        await writeFile(target, 'gone during reconcile')
        objects.set(objectKey, Buffer.from('stale bytes'))
        objects.set(backup.manifestKey(objectKey), Buffer.from('{}'))
        client.list = async (...args) => {
            await rm(target, { force: true })
            return originalList.apply(client, args)
        }

        try {
            await expect(fylo.reconcile()).resolves.toBeUndefined()
            expect(objects.has(objectKey)).toBe(false)
            expect(objects.has(backup.manifestKey(objectKey))).toBe(false)
        } finally {
            client.list = originalList
            await rm(target, { force: true })
        }
    })

    test('encodes every xattr beside the bytes so a backup download is recoverable', async () => {
        const id = await fylo.assets.put(new File(['recover me'], 'recover.txt'), {
            key: '/archive/recover.txt',
            meta: { owner: 'backup-test' }
        })
        await fylo.reconcile()

        const dataKey = [...objects.keys()].find(
            (key) => key.includes('.buckets/assets/docs/') && key.includes(String(id))
        )
        expect(dataKey).toBeTruthy()
        const manifest = JSON.parse(
            new TextDecoder().decode(
                /** @type {Buffer} */ (xattrManifestFor(/** @type {string} */ (dataKey)))
            )
        )
        expect(manifest.version).toBe(2)
        expect(manifest.platform).toBe(process.platform === 'win32' ? 'windows-ntfs' : 'posix')
        expect(manifest.native).toMatchObject({
            mode: expect.any(Number),
            mtimeMs: expect.any(Number)
        })
        expect(manifest.dataKey).toBe(dataKey)
        expect(Object.keys(manifest.xattrs)).toContain('user.fylo.key')
        expect(Object.keys(manifest.xattrs)).toContain('user.fylo.meta.owner')
        expect(
            new TextDecoder().decode(
                Uint8Array.from(Buffer.from(manifest.xattrs['user.fylo.key'], 'base64'))
            )
        ).toBe('/archive/recover.txt')
    })

    test('metadata-only writes and rekeys immediately refresh the xattr manifest', async () => {
        const id = await fylo.assets.put(new File(['metadata'], 'metadata.txt'), {
            key: '/before.txt'
        })
        writes.length = 0

        await fylo.assets.put(id).metadata({ reviewed: true })
        expect(writes.some((key) => key.includes('.fylo-backup/xattrs/'))).toBe(true)

        writes.length = 0
        await fylo.assets.rekey(id, '/after.txt')
        expect(writes.some((key) => key.includes('.fylo-backup/xattrs/'))).toBe(true)
    })

    test('reconcile detects changed bytes even when file size is unchanged', async () => {
        const id = await fylo.users.put({ name: 'same-size-A' })
        const target = fylo.engine.docPath('users', String(id))
        await fylo.reconcile()
        const key = fylo.engine.backup.key(target)
        const before = Buffer.from(/** @type {Buffer} */ (objects.get(key)))
        const changed = before.toString().replace('same-size-A', 'same-size-B')
        expect(Buffer.byteLength(changed)).toBe(before.length)
        await writeFile(target, changed)

        await fylo.reconcile()
        expect(objects.get(key)?.toString()).toBe(changed)
    })

    test('reconcile never uploads bytes from an active transaction that rolls back', async () => {
        const collection = `backup-transaction-${Date.now()}`
        await fylo[collection].create()
        const id = await fylo[collection].put({ state: 'committed' })
        await fylo.reconcile()
        const uploadedBodies = []
        const client = fylo.engine.backup.client
        const originalWrite = client.write.bind(client)
        client.write = async (key, body) => {
            uploadedBodies.push(Buffer.isBuffer(body) ? body.toString() : String(body))
            return await originalWrite(key, body)
        }
        const updateDocument = fylo.engine.updateDocument.bind(fylo.engine)
        let mutationStarted
        const started = new Promise((resolve) => {
            mutationStarted = resolve
        })
        let releaseMutation
        const release = new Promise((resolve) => {
            releaseMutation = resolve
        })
        fylo.engine.updateDocument = async (...args) => {
            const result = await updateDocument(...args)
            mutationStarted()
            await release
            throw new Error('rollback after transient bytes')
        }
        try {
            const mutation = fylo
                ._sql(`UPDATE ${collection} SET state = 'uncommitted' WHERE state = 'committed'`)
                .catch((error) => error)
            await started
            const reconcile = fylo.reconcile()
            await Bun.sleep(20)
            releaseMutation()
            expect(await mutation).toBeInstanceOf(Error)
            await reconcile
            expect(uploadedBodies.some((body) => body.includes('uncommitted'))).toBe(false)
            expect((await fylo[collection].get(id).once())[id]).toEqual({ state: 'committed' })
        } finally {
            fylo.engine.updateDocument = updateDocument
            client.write = originalWrite
        }
    })

    test('one reconcile generation is immutable even when local generation advances mid-upload', async () => {
        const collection = `backup-generation-${Date.now()}`
        await fylo[collection].create()
        const ids = [
            await fylo[collection].put({ state: 'old', ordinal: 1 }),
            await fylo[collection].put({ state: 'old', ordinal: 2 })
        ]
        await fylo.reconcile()
        const backup = fylo.engine.backup
        const dataKeys = ids.map((id) => backup.key(fylo.engine.docPath(collection, String(id))))
        for (const key of dataKeys) {
            objects.delete(key)
            for (const [candidate, bytes] of objects) {
                if (
                    candidate.includes('.fylo-backup/xattrs/') &&
                    JSON.parse(bytes.toString()).dataKey === key
                ) {
                    objects.delete(candidate)
                }
            }
        }
        const statePath = path.join(
            root,
            '.fylo-transactions',
            '.collections',
            collection,
            'state.json'
        )
        const client = backup.client
        const originalWrite = client.write.bind(client)
        let advanced = false
        client.write = async (key, body) => {
            if (!advanced && dataKeys.includes(key)) {
                advanced = true
                for (const id of ids) {
                    const target = fylo.engine.docPath(collection, String(id))
                    const changed = (await readFile(target, 'utf8')).replace('"old"', '"new"')
                    await writeFile(target, changed)
                }
                const state = JSON.parse(await readFile(statePath, 'utf8'))
                await writeFile(
                    statePath,
                    JSON.stringify({ ...state, generation: state.generation + 2 })
                )
            }
            return await originalWrite(key, body)
        }
        try {
            await fylo.reconcile()
        } finally {
            client.write = originalWrite
        }
        expect(advanced).toBe(true)
        const persistedStates = dataKeys.map((key) => JSON.parse(objects.get(key).toString()).state)
        expect(persistedStates).toEqual(['old', 'old'])
    })

    test('reconcile rejects symlinked, oversized, and non-canonical generation state', async () => {
        const collection = `backup-state-${Date.now()}`
        await fylo[collection].create()
        await fylo[collection].put({ state: 'known' })
        const statePath = path.join(
            root,
            '.fylo-transactions',
            '.collections',
            collection,
            'state.json'
        )
        const saved = `${statePath}.saved`
        const outsideDirectory = await mkdtemp(path.join(os.tmpdir(), 'fylo-backup-state-'))
        const outsideState = path.join(outsideDirectory, 'state.json')
        const original = await readFile(statePath)

        try {
            await rename(statePath, saved)
            await writeFile(outsideState, original)
            await symlink(outsideState, statePath)
            await expect(fylo.reconcile()).rejects.toThrow(/Secure open failed|symbolic link/)
            expect(await readFile(outsideState)).toEqual(original)
            await rm(statePath)
            await rename(saved, statePath)

            const descriptor = await open(statePath, 'w')
            await descriptor.truncate(16 * 1024 + 1)
            await descriptor.close()
            await expect(fylo.reconcile()).rejects.toThrow('generation state exceeds bounds')
            await writeFile(statePath, original)

            const state = JSON.parse(original.toString())
            await writeFile(statePath, JSON.stringify({ ...state, unexpected: true }))
            await expect(fylo.reconcile()).rejects.toThrow('generation state is corrupt')
            await writeFile(statePath, original)
        } finally {
            await rm(statePath, { force: true })
            if (await Bun.file(saved).exists()) await rename(saved, statePath)
            else await writeFile(statePath, original)
            await rm(outsideDirectory, { recursive: true, force: true })
        }
    })

    test('a rejected reconcile does not create a second unhandled rejection', async () => {
        const client = fylo.engine.backup.client
        const originalList = client.list
        client.list = async () => {
            throw new Error('list failed')
        }
        /** @type {unknown[]} */
        const unhandled = []
        const onUnhandled = (reason) => unhandled.push(reason)
        process.on('unhandledRejection', onUnhandled)
        try {
            await expect(fylo.reconcile()).rejects.toThrow('list failed')
            await Promise.resolve()
            await Bun.sleep(0)
            expect(unhandled).toEqual([])
        } finally {
            process.off('unhandledRejection', onUnhandled)
            client.list = originalList
        }
    })
})
