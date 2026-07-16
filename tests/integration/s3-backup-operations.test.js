import { afterEach, describe, expect, test } from 'bun:test'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { FyloS3Backup } from '../../src/replication/s3-backup.js'

const roots = []

async function root() {
    const value = await mkdtemp(path.join(os.tmpdir(), 'fylo-s3-operations-'))
    roots.push(value)
    return value
}

function deferred() {
    let resolve
    const promise = new Promise((done) => (resolve = done))
    return { promise, resolve }
}

class OperationalClient {
    objects = new Map()
    listCalls = 0
    active = 0
    maximumActive = 0
    failures = 0
    gate

    async request(task) {
        this.active++
        this.maximumActive = Math.max(this.maximumActive, this.active)
        try {
            if (this.gate) await this.gate.promise
            return task()
        } finally {
            this.active--
        }
    }

    async write(key, body) {
        return this.request(() => this.objects.set(key, Buffer.from(body)))
    }

    async delete(key) {
        return this.request(() => this.objects.delete(key))
    }

    async list({ prefix = '', startAfter } = {}) {
        this.listCalls++
        if (this.failures-- > 0) {
            const error = new Error('temporary S3 outage')
            error.code = 'ECONNRESET'
            throw error
        }
        const keys = [...this.objects.keys()].filter((key) => key.startsWith(prefix)).sort()
        const remaining = startAfter ? keys.filter((key) => key > startAfter) : keys
        const page = remaining.slice(0, 1)
        return this.request(() => ({
            contents: page.map((key) => ({ key, size: this.objects.get(key).length })),
            isTruncated: remaining.length > page.length
        }))
    }

    file(key) {
        return {
            stream: () => {
                const value = this.objects.get(key)
                if (!value) throw new Error(`missing ${key}`)
                return new Blob([value]).stream()
            }
        }
    }
}

afterEach(async () => {
    await Promise.all(roots.splice(0).map((value) => rm(value, { recursive: true, force: true })))
})

describe('S3 backup operational controls', () => {
    test('paginates listings and bounds all simultaneous S3 requests', async () => {
        const directory = await root()
        const files = await Promise.all(
            Array.from({ length: 6 }, async (_, index) => {
                const target = path.join(directory, `${index}.txt`)
                await writeFile(target, `file-${index}`)
                return target
            })
        )
        const client = new OperationalClient()
        client.objects.set('tenant/orphan-a', Buffer.from('a'))
        client.objects.set('tenant/orphan-b', Buffer.from('b'))
        const backup = new FyloS3Backup(
            { bucket: 'backup', prefix: 'tenant', concurrency: 2 },
            directory,
            { client }
        )

        await backup.mirror(files)
        await backup.reconcile()

        expect(client.listCalls).toBeGreaterThan(1)
        expect(client.maximumActive).toBeLessThanOrEqual(2)
        expect(client.objects.has('tenant/orphan-a')).toBe(false)
        expect(client.objects.has('tenant/orphan-b')).toBe(false)
        await backup.close()
    })

    test('reports retry exhaustion and recovers on the next manual pass', async () => {
        const directory = await root()
        const client = new OperationalClient()
        client.failures = 2
        const events = []
        const backup = new FyloS3Backup(
            {
                bucket: 'backup',
                prefix: 'tenant',
                retry: { attempts: 2, baseDelayMs: 0, maxDelayMs: 0 }
            },
            directory,
            { client, onEvent: (event) => events.push(event), random: () => 0 }
        )

        await expect(backup.reconcile()).rejects.toThrow('temporary S3 outage')
        expect(backup.status.state).toBe('failed')
        await expect(backup.reconcile()).resolves.toBeUndefined()
        expect(backup.status.state).toBe('idle')
        expect(backup.status.lastSuccessAt).toBeTruthy()
        expect(events.filter((event) => event.type === 'backup.retry')).toHaveLength(1)
        await backup.close()
    })

    test('emits scheduled lifecycle failures with actionable status', async () => {
        const directory = await root()
        const client = new OperationalClient()
        client.failures = 1
        const events = []
        const backup = new FyloS3Backup(
            { bucket: 'backup', prefix: 'tenant', retry: { attempts: 1 } },
            directory,
            { client, onEvent: (event) => events.push(event) }
        )

        await expect(backup.reconcile('scheduled')).rejects.toThrow('temporary S3 outage')
        expect(events.map((event) => [event.type, event.source])).toEqual([
            ['backup.reconcile.started', 'scheduled'],
            ['backup.reconcile.failed', 'scheduled']
        ])
        expect(backup.status.lastError).toBe('temporary S3 outage')
        await backup.close()
    })

    test('close cancels queued requests, drains the active request, and never reopens the root', async () => {
        const directory = await root()
        const target = path.join(directory, 'active.txt')
        await writeFile(target, 'active')
        const client = new OperationalClient()
        client.gate = deferred()
        const backup = new FyloS3Backup(
            { bucket: 'backup', prefix: 'tenant', concurrency: 1 },
            directory,
            { client }
        )

        const mirror = backup.mirror([target])
        while (client.active === 0) await Bun.sleep(0)
        const closing = backup.close()
        client.gate.resolve()

        await expect(mirror).rejects.toThrow('closing')
        await expect(closing).resolves.toBeUndefined()
        expect(backup.rootFd).toBeUndefined()
        await expect(backup.mirror([target])).rejects.toThrow('closed')
        expect(backup.rootFd).toBeUndefined()
    })

    test('orders mirror-on-write ahead of a concurrent full reconcile', async () => {
        const directory = await root()
        const target = path.join(directory, 'ordered.txt')
        await writeFile(target, 'ordered')
        const client = new OperationalClient()
        client.gate = deferred()
        const backup = new FyloS3Backup(
            { bucket: 'backup', prefix: 'tenant', concurrency: 2 },
            directory,
            { client }
        )

        const mirror = backup.mirror([target])
        while (client.active === 0) await Bun.sleep(0)
        const reconcile = backup.reconcile()
        await Bun.sleep(0)
        expect(client.listCalls).toBe(0)

        client.gate.resolve()
        await Promise.all([mirror, reconcile])
        expect(client.listCalls).toBeGreaterThan(0)
        await backup.close()
    })

    test('rejects oversized files before upload', async () => {
        const directory = await root()
        const target = path.join(directory, 'large.txt')
        await writeFile(target, '12345')
        const client = new OperationalClient()
        const backup = new FyloS3Backup(
            { bucket: 'backup', prefix: 'tenant', maxFileBytes: 4 },
            directory,
            { client }
        )

        await expect(backup.mirror([target])).rejects.toThrow('maxFileBytes')
        expect(client.objects.size).toBe(0)
        await expect(backup.reconcile()).rejects.toThrow('maxFileBytes')
        expect(client.maximumActive).toBeLessThanOrEqual(1)
        await backup.close()
    })

    test('rejects oversized remote manifests before downloading them', async () => {
        const directory = await root()
        const target = path.join(directory, 'bounded.txt')
        await writeFile(target, 'data')
        const client = new OperationalClient()
        const backup = new FyloS3Backup(
            { bucket: 'backup', prefix: 'tenant', maxManifestBytes: 4 },
            directory,
            { client }
        )
        const key = backup.key(target)
        client.objects.set(key, Buffer.from('data'))
        client.objects.set(backup.manifestKey(key), Buffer.from('too-large'))

        await expect(backup.reconcile()).rejects.toThrow('maxManifestBytes')
        await backup.close()
    })

    test.each([
        ['missing', undefined],
        ['underreported', 1]
    ])(
        'bounds a streamed remote manifest when its listed size is %s',
        async (_description, listedSize) => {
            const directory = await root()
            const target = path.join(directory, 'bounded-stream.txt')
            await writeFile(target, 'data')
            const client = new OperationalClient()
            const backup = new FyloS3Backup(
                { bucket: 'backup', prefix: 'tenant', maxManifestBytes: 4 },
                directory,
                { client }
            )
            const key = backup.key(target)
            const manifestKey = backup.manifestKey(key)
            let cancelled = false
            client.objects.set(key, Buffer.from('data'))
            client.list = async () => ({
                contents: [
                    { key, size: 4 },
                    { key: manifestKey, ...(listedSize === undefined ? {} : { size: listedSize }) }
                ],
                isTruncated: false
            })
            client.file = () => ({
                arrayBuffer: async () => {
                    throw new Error('unbounded arrayBuffer must not be used')
                },
                stream: () =>
                    new ReadableStream({
                        pull(controller) {
                            controller.enqueue(new Uint8Array([1, 2, 3]))
                        },
                        cancel() {
                            cancelled = true
                        }
                    })
            })

            await expect(backup.reconcile()).rejects.toThrow('maxManifestBytes')
            expect(cancelled).toBe(true)
            await backup.close()
        }
    )
})
