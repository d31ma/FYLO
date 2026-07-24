import { afterEach, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { FyloS3Restore } from '../../src/replication/s3-restore.js'
import { getXattr } from '../../src/storage/xattr.js'
import { acquireRootReservation } from '../../src/cli/root-lease.js'

/** @type {string[]} */
const cleanup = []

afterEach(async () => {
    await Promise.all(
        cleanup.splice(0).map((target) => rm(target, { recursive: true, force: true }))
    )
})

function manifest(dataKey, bytes, xattrs = {}) {
    return Buffer.from(
        JSON.stringify({
            version: process.platform === 'win32' ? 2 : 1,
            ...(process.platform === 'win32'
                ? {
                      platform: 'windows-ntfs',
                      native: { mode: 0o600, mtimeMs: Date.now() }
                  }
                : {}),
            dataKey,
            size: bytes.byteLength,
            sha256: createHash('sha256').update(bytes).digest('hex'),
            xattrs
        })
    )
}

function platformManifest(dataKey, bytes, platform) {
    return Buffer.from(
        JSON.stringify({
            version: 2,
            platform,
            dataKey,
            size: bytes.byteLength,
            sha256: createHash('sha256').update(bytes).digest('hex'),
            xattrs: {},
            native: { mode: 0o600, mtimeMs: Date.now() }
        })
    )
}

function manifestKey(prefix, dataKey) {
    return `${prefix}/.fylo-backup/xattrs/${Buffer.from(dataKey).toString('base64url')}.json`
}

class RestoreClient {
    constructor(
        objects,
        { pageSize = 2, transientReads = new Map(), readDelayMs = 0, readDelays = new Map() } = {}
    ) {
        this.objects = objects
        this.pageSize = pageSize
        this.transientReads = transientReads
        this.readDelayMs = readDelayMs
        this.readDelays = readDelays
        this.listCalls = 0
        this.activeReads = 0
        this.maxActiveReads = 0
        this.streamedKeys = []
    }

    async list({ prefix = '', startAfter } = {}) {
        this.listCalls++
        const keys = [...this.objects.keys()]
            .filter((key) => key.startsWith(prefix) && (!startAfter || key > startAfter))
            .sort()
        const page = keys.slice(0, this.pageSize)
        return {
            contents: page.map((key) => ({ key, size: this.objects.get(key).byteLength })),
            isTruncated: keys.length > page.length
        }
    }

    file(key) {
        return {
            stream: () => {
                const client = this
                return new ReadableStream({
                    async start(controller) {
                        client.streamedKeys.push(key)
                        const failures = client.transientReads.get(key) ?? 0
                        if (failures > 0) {
                            client.transientReads.set(key, failures - 1)
                            controller.error(
                                Object.assign(new Error('temporary S3 failure'), {
                                    code: 'SlowDown'
                                })
                            )
                            return
                        }
                        const bytes = client.objects.get(key)
                        if (!bytes) {
                            controller.error(
                                Object.assign(new Error(`Missing object: ${key}`), {
                                    code: 'NoSuchKey'
                                })
                            )
                            return
                        }
                        client.activeReads++
                        client.maxActiveReads = Math.max(client.maxActiveReads, client.activeReads)
                        const delay = client.readDelays.get(key) ?? client.readDelayMs
                        if (delay) await Bun.sleep(delay)
                        controller.enqueue(bytes)
                        controller.close()
                        client.activeReads--
                    }
                })
            }
        }
    }
}

async function fixture(files, clientOptions) {
    const parent = await mkdtemp(path.join(os.tmpdir(), 'fylo-s3-restore-'))
    cleanup.push(parent)
    const root = path.join(parent, 'restored')
    const prefix = 'tenant-a'
    const objects = new Map()
    for (const [relative, value] of Object.entries(files)) {
        const bytes = Buffer.from(value.bytes)
        const dataKey = `${prefix}/${relative}`
        objects.set(dataKey, bytes)
        objects.set(manifestKey(prefix, dataKey), manifest(dataKey, bytes, value.xattrs))
    }
    const client = new RestoreClient(objects, clientOptions)
    return { parent, root, prefix, objects, client }
}

describe('S3 recovery', () => {
    test('restores paginated objects with xattrs and atomically promotes the staging root', async () => {
        const keyXattr = Buffer.from('/hello.txt').toString('base64')
        const accessXattr = Buffer.from(
            JSON.stringify({
                version: 1,
                uid: process.getuid?.() ?? 0,
                gid: process.getgid?.() ?? 0,
                mode: 0o600
            })
        ).toString('base64')
        const { root, prefix, client } = await fixture({
            'catalog.json': { bytes: '{"format":1}' },
            '.buckets/assets/docs/hello': {
                bytes: 'hello',
                xattrs: {
                    'user.fylo.key': keyXattr,
                    ...(process.platform === 'win32' ? {} : { 'user.fylo.access': accessXattr })
                }
            }
        })
        const events = []
        const restore = new FyloS3Restore({ bucket: 'backup', prefix }, root, { client })

        const result = await restore.restore({ onStatus: (event) => events.push(event) })

        expect(await readFile(path.join(root, 'catalog.json'), 'utf8')).toBe('{"format":1}')
        const restoredFile = path.join(root, '.buckets/assets/docs/hello')
        expect(await readFile(restoredFile, 'utf8')).toBe('hello')
        expect(Buffer.from(getXattr(restoredFile, 'user.fylo.key')).toString()).toBe('/hello.txt')
        const access = await stat(restoredFile)
        expect(access.uid).toBe(process.getuid?.() ?? 0)
        // NTFS projects only the read-only bit: a writable file reports 0o666.
        expect(access.mode & 0o777).toBe(process.platform === 'win32' ? 0o666 : 0o600)
        expect(client.listCalls).toBeGreaterThan(1)
        expect(result).toMatchObject({ status: 'complete', files: 2, bytes: 17 })
        expect(events.map((event) => event.phase)).toContain('promote')
        expect(events.at(-1)?.phase).toBe('complete')
    })

    test('fails before contacting S3 when the destination already exists', async () => {
        const { root, prefix, client } = await fixture({ 'one.txt': { bytes: 'one' } })
        await mkdir(root)
        await writeFile(path.join(root, 'keep.txt'), 'do not replace')
        const restore = new FyloS3Restore({ bucket: 'backup', prefix }, root, { client })

        await expect(restore.restore()).rejects.toThrow('destination already exists')
        expect(await readFile(path.join(root, 'keep.txt'), 'utf8')).toBe('do not replace')
        expect(client.listCalls).toBe(0)
    })

    test('refuses a destination reserved by another live root owner', async () => {
        const { root, prefix, client } = await fixture({ 'one.txt': { bytes: 'one' } })
        const owner = await acquireRootReservation(root)
        try {
            const restore = new FyloS3Restore({ bucket: 'backup', prefix }, root, { client })
            await expect(restore.restore()).rejects.toMatchObject({ code: 'EROOTLOCKED' })
            expect(client.listCalls).toBe(0)
        } finally {
            await owner.release()
        }
    })

    test('rejects a backup manifest created for a different filesystem family', async () => {
        const { root, prefix, objects, client } = await fixture({})
        const dataKey = `${prefix}/one.txt`
        const bytes = Buffer.from('one')
        objects.set(dataKey, bytes)
        objects.set(
            manifestKey(prefix, dataKey),
            platformManifest(
                dataKey,
                bytes,
                process.platform === 'win32' ? 'posix' : 'windows-ntfs'
            )
        )
        const restore = new FyloS3Restore({ bucket: 'backup', prefix }, root, { client })

        await expect(restore.verify()).rejects.toThrow('incompatible')
        expect(await Bun.file(root).exists()).toBe(false)
    })

    test('rejects traversal keys and leaves neither destination nor staging data', async () => {
        const { parent, root, prefix, objects, client } = await fixture({})
        const dataKey = `${prefix}/../outside.txt`
        const bytes = Buffer.from('escape')
        objects.set(dataKey, bytes)
        objects.set(manifestKey(prefix, dataKey), manifest(dataKey, bytes))
        const restore = new FyloS3Restore({ bucket: 'backup', prefix }, root, { client })

        await expect(restore.restore()).rejects.toThrow('unsafe S3 object key')
        expect(await Bun.file(path.join(parent, 'outside.txt')).exists()).toBe(false)
        expect(await Bun.file(root).exists()).toBe(false)
        expect(
            await Array.fromAsync(new Bun.Glob('restored.fylo-restore-*.tmp').scan(parent))
        ).toEqual([])
    })

    test('validates the manifest checksum and cleans the staging root on failure', async () => {
        const { parent, root, prefix, objects, client } = await fixture({
            'corrupt.txt': { bytes: 'expected' }
        })
        objects.set(`${prefix}/corrupt.txt`, Buffer.from('tampered'))
        const restore = new FyloS3Restore({ bucket: 'backup', prefix }, root, { client })

        await expect(restore.restore()).rejects.toThrow('checksum mismatch')
        expect(await Bun.file(root).exists()).toBe(false)
        expect(
            await Array.fromAsync(new Bun.Glob('restored.fylo-restore-*.tmp').scan(parent))
        ).toEqual([])
    })

    test('drains active workers before cleaning staging after the first failure', async () => {
        const restorePrefix = 'tenant-a'
        const { parent, root, prefix, objects, client } = await fixture(
            {
                'a-corrupt.txt': { bytes: 'expected' },
                'b-slow.txt': { bytes: 'slow' },
                'c-never-started.txt': { bytes: 'pending' }
            },
            {
                pageSize: 20,
                readDelays: new Map([
                    [`${restorePrefix}/a-corrupt.txt`, 5],
                    [`${restorePrefix}/b-slow.txt`, 50]
                ])
            }
        )
        objects.set(`${prefix}/a-corrupt.txt`, Buffer.from('tampered'))
        const restore = new FyloS3Restore({ bucket: 'backup', prefix }, root, { client })

        await expect(restore.restore({ concurrency: 2 })).rejects.toThrow('checksum mismatch')

        expect(client.activeReads).toBe(0)
        expect(client.streamedKeys).not.toContain(`${prefix}/c-never-started.txt`)
        expect(
            await Array.fromAsync(new Bun.Glob('restored.fylo-restore-*.tmp').scan(parent))
        ).toEqual([])
    })

    test('bounds concurrent downloads and retries transient reads', async () => {
        const { root, prefix, client } = await fixture(
            {
                'one.txt': { bytes: 'one' },
                'two.txt': { bytes: 'two' },
                'three.txt': { bytes: 'three' }
            },
            { pageSize: 20, readDelayMs: 5 }
        )
        client.transientReads.set(`${prefix}/two.txt`, 1)
        const statuses = []
        const restore = new FyloS3Restore({ bucket: 'backup', prefix }, root, { client })

        await restore.restore({
            concurrency: 2,
            retry: { attempts: 3, baseDelayMs: 0 },
            onStatus: (event) => statuses.push(event)
        })

        expect(client.maxActiveReads).toBeLessThanOrEqual(2)
        expect(statuses.some((event) => event.phase === 'retry')).toBe(true)
        expect(await readFile(path.join(root, 'two.txt'), 'utf8')).toBe('two')
    })

    test('verify streams and validates the backup without creating a destination', async () => {
        const { root, prefix, client } = await fixture({ 'one.txt': { bytes: 'one' } })
        const restore = new FyloS3Restore({ bucket: 'backup', prefix }, root, { client })

        const result = await restore.verify()

        expect(result).toMatchObject({ status: 'verified', files: 1, bytes: 3 })
        expect(await Bun.file(root).exists()).toBe(false)
    })

    test('fails closed when the selected prefix contains no data objects', async () => {
        const { root, prefix, client } = await fixture({})
        const restore = new FyloS3Restore({ bucket: 'backup', prefix }, root, { client })

        await expect(restore.restore()).rejects.toThrow('contains no data objects')
        expect(await Bun.file(root).exists()).toBe(false)
    })
})
