import { afterAll, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createInterface } from 'node:readline'
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

const live = process.env.FYLO_REQUIRE_LIVE_S3 === '1'
const binary = path.resolve(process.env.FYLO_BINARY ?? 'dist-bin/fylo')
const bucket = process.env.FYLO_S3_BUCKET ?? 'fylo-live-gate'
const prefix = process.env.FYLO_S3_PREFIX ?? `release-gate/${Date.now()}`
const endpoint = process.env.FYLO_S3_ENDPOINT ?? 'http://127.0.0.1:9000'
const region = process.env.FYLO_S3_REGION ?? 'us-east-1'
const roots = []

async function temporaryRoot(name) {
    const root = await mkdtemp(path.join(os.tmpdir(), name))
    roots.push(root)
    return root
}

async function runBinary(args) {
    const process = Bun.spawn([binary, ...args], {
        env: { ...globalThis.process.env },
        stdin: 'ignore',
        stdout: 'pipe',
        stderr: 'pipe'
    })
    const [stdout, stderr, exitCode] = await Promise.all([
        new Response(process.stdout).text(),
        new Response(process.stderr).text(),
        process.exited
    ])
    return { stdout, stderr, exitCode }
}

class MachineLoop {
    constructor(root) {
        this.child = spawn(
            binary,
            [
                'exec',
                '--loop',
                '--root',
                root,
                '--exclusive-root',
                '--backup-bucket',
                bucket,
                '--backup-prefix',
                prefix,
                '--backup-endpoint',
                endpoint,
                '--backup-region',
                region,
                '--backup-concurrency',
                '4'
            ],
            {
                env: { ...process.env },
                stdio: ['pipe', 'pipe', 'pipe']
            }
        )
        this.responses = []
        this.waiters = []
        this.stderr = ''
        this.child.stderr.setEncoding('utf8')
        this.child.stderr.on('data', (chunk) => {
            this.stderr += chunk
        })
        this.reader = createInterface({ input: this.child.stdout })
        this.reader.on('line', (line) => {
            const response = JSON.parse(line)
            const waiter = this.waiters.shift()
            if (waiter) waiter.resolve(response)
            else this.responses.push(response)
        })
        this.child.on('exit', () => {
            const failure = new Error(`FYLO loop exited before responding: ${this.stderr}`)
            for (const waiter of this.waiters.splice(0)) waiter.reject(failure)
        })
    }

    async request(request) {
        this.child.stdin.write(`${JSON.stringify(request)}\n`)
        if (this.responses.length > 0) return this.responses.shift()
        return await new Promise((resolve, reject) => this.waiters.push({ resolve, reject }))
    }

    async close() {
        this.child.stdin.end()
        await once(this.child, 'exit')
        expect(this.child.exitCode).toBe(0)
    }
}

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe.skipIf(!live)('live S3-compatible compiled-binary recovery gate', () => {
    test('mirrors, reconciles, verifies, restores, and detects provider corruption', async () => {
        const root = await temporaryRoot('fylo-live-s3-root-')
        const destinationParent = await temporaryRoot('fylo-live-s3-restore-parent-')
        const destination = path.join(destinationParent, 'restored')
        const fixtures = path.join(root, 'release-gate-fixtures')
        await mkdir(fixtures)
        await Promise.all(
            Array.from({ length: 520 }, (_, index) =>
                writeFile(
                    path.join(fixtures, `${String(index).padStart(4, '0')}.txt`),
                    `fixture-${index}`
                )
            )
        )
        const upload = path.join(root, 'binary-upload.txt')
        await writeFile(upload, 'binary file payload')

        const loop = new MachineLoop(root)
        expect(
            (await loop.request({ op: 'handshake' })).result.capabilities.wholeRootBackup
        ).toMatchObject({ available: true, configured: true })
        expect(
            await loop.request({ op: 'createCollection', collection: 'docs', kind: 'document' })
        ).toMatchObject({ ok: true })
        expect(
            await loop.request({ op: 'createCollection', collection: 'files', kind: 'file' })
        ).toMatchObject({ ok: true })
        const inserted = await loop.request({
            op: 'putData',
            collection: 'docs',
            data: { title: 'live backup', generation: 1 }
        })
        expect(inserted.ok).toBe(true)
        const deletedId = String(inserted.result)
        const retained = await loop.request({
            op: 'putData',
            collection: 'docs',
            data: { title: 'retained', generation: 2 },
            meta: { releaseGate: true }
        })
        expect(retained.ok).toBe(true)
        expect(
            await loop.request({
                op: 'putData',
                collection: 'files',
                file: { path: upload, key: '/release/binary-upload.txt' },
                meta: { releaseGate: true }
            })
        ).toMatchObject({ ok: true })
        expect(
            await loop.request({ op: 'delDoc', collection: 'docs', id: deletedId })
        ).toMatchObject({ ok: true })
        const reconciled = await loop.request({ op: 'backupReconcile' })
        expect(reconciled).toMatchObject({
            ok: true,
            result: { state: 'idle', runs: expect.any(Number) }
        })
        const status = await loop.request({ op: 'backupStatus' })
        expect(status.result.lastSuccessAt).toBeTruthy()
        await loop.close()

        const recoveryArgs = [
            '--backup-bucket',
            bucket,
            '--backup-prefix',
            prefix,
            '--backup-endpoint',
            endpoint,
            '--backup-region',
            region,
            '--json'
        ]
        const verified = await runBinary(['backup', 'verify', ...recoveryArgs])
        expect(verified.exitCode).toBe(0)
        expect(JSON.parse(verified.stdout).files).toBeGreaterThan(520)

        const restored = await runBinary([
            'backup',
            'restore',
            ...recoveryArgs,
            '--destination',
            destination
        ])
        expect(restored.exitCode).toBe(0)
        expect(
            await readFile(path.join(destination, 'release-gate-fixtures/0519.txt'), 'utf8')
        ).toBe('fixture-519')

        const deleted = await runBinary([
            'exec',
            '--root',
            destination,
            '--request',
            JSON.stringify({
                op: 'findDeletedDocs',
                collection: 'docs',
                query: { $onlyIds: true }
            })
        ])
        expect(deleted.exitCode).toBe(0)
        expect(JSON.parse(deleted.stdout).result).toContain(deletedId)

        const client = new Bun.S3Client({
            accessKeyId: process.env.AWS_ACCESS_KEY_ID,
            secretAccessKey: process.env.AWS_SECRET_ACCESS_KEY,
            endpoint,
            region,
            bucket
        })
        const listing = await client.list({ prefix: `${prefix}/release-gate-fixtures/` })
        const corruptKey = listing.contents?.find((entry) => entry.key)?.key
        expect(corruptKey).toBeTruthy()
        await client.write(corruptKey, 'provider-corruption')
        const corrupt = await runBinary(['backup', 'verify', ...recoveryArgs])
        expect(corrupt.exitCode).not.toBe(0)
        expect(corrupt.stderr).toContain('mismatch')

        const digest = createHash('sha256')
            .update(await readFile(binary))
            .digest('hex')
        console.log(
            JSON.stringify({
                artifact: path.basename(binary),
                sha256: digest,
                host: `${process.platform}-${process.arch}`,
                s3: 'MinIO',
                objects: JSON.parse(verified.stdout).files,
                prefix
            })
        )
    }, 300_000)
})
