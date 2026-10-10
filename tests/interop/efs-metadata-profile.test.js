// Source-first EFS profile contract. Requires an explicitly admitted candidate.
// These local tests are not independent-client NFS/EFS qualification.
import { afterAll, describe, expect, test } from 'bun:test'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createInterface } from 'node:readline'
import { createHash } from 'node:crypto'
import { cp, mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

const required = process.env.FYLO_REQUIRE_EFS_PROFILE === '1'
const binary = path.resolve(process.env.FYLO_BINARY ?? 'dist-bin/fylo')
const profile = 'efs-sidecar-v1'
const id = '4VRNF52JPCO'
const rawId = '4VRNF52JPCP'
const roots = []
const loops = new Set()

class ProfileLoop {
    constructor(root, { selectedProfile = profile, env = {} } = {}) {
        const args = ['exec', '--loop', '--root', root, '--exclusive-root']
        if (selectedProfile !== null) args.push('--filesystem-profile', selectedProfile)
        this.child = spawn(binary, args, {
            stdio: ['pipe', 'pipe', 'pipe'],
            env: { ...process.env, ...env }
        })
        this.waiters = []
        this.responses = []
        this.stderr = ''
        this.exited = once(this.child, 'exit')
        this.child.stderr.setEncoding('utf8')
        this.child.stderr.on('data', (chunk) => {
            this.stderr += chunk
        })
        this.reader = createInterface({ input: this.child.stdout })
        this.reader.on('line', (line) => {
            try {
                const response = JSON.parse(line)
                const waiter = this.waiters.shift()
                if (waiter) waiter.resolve(response)
                else this.responses.push(response)
            } catch (error) {
                this.rejectPending(error)
            }
        })
        this.child.on('error', (error) => this.rejectPending(error))
        this.child.stdin.on('error', (error) => this.rejectPending(error))
        this.child.on('exit', () => {
            this.rejectPending(new Error('machine exited: ' + this.stderr))
            loops.delete(this)
        })
        loops.add(this)
    }

    rejectPending(error) {
        for (const waiter of this.waiters.splice(0)) waiter.reject(error)
    }

    async request(request) {
        if (this.child.exitCode !== null || this.child.signalCode !== null) {
            throw new Error('machine already exited: ' + this.stderr)
        }
        const response = this.responses.length
            ? Promise.resolve(this.responses.shift())
            : new Promise((resolve, reject) => this.waiters.push({ resolve, reject }))
        this.child.stdin.write(JSON.stringify(request) + '\n')
        let timer
        try {
            return await Promise.race([
                response,
                new Promise((_, reject) => {
                    timer = setTimeout(() => reject(new Error('machine request timeout')), 5_000)
                })
            ])
        } finally {
            clearTimeout(timer)
        }
    }

    async result(request) {
        const response = await this.request(request)
        expect(response.ok, JSON.stringify(response)).toBe(true)
        return response.result
    }

    async close() {
        if (this.child.exitCode !== null || this.child.signalCode !== null) return
        this.child.stdin.end()
        let timer
        try {
            await Promise.race([
                this.exited,
                new Promise((_, reject) => {
                    timer = setTimeout(() => {
                        this.child.kill('SIGKILL')
                        reject(new Error('machine failed to close'))
                    }, 5_000)
                })
            ])
        } finally {
            clearTimeout(timer)
        }
    }
}

async function freshRoot() {
    const root = await mkdtemp(path.join(os.tmpdir(), 'fylo-efs-contract-'))
    roots.push(root)
    return root
}

async function filesUnder(root) {
    const entries = []
    for (const entry of await readdir(root, { withFileTypes: true })) {
        const current = path.join(root, entry.name)
        if (entry.isDirectory()) entries.push(...(await filesUnder(current)))
        else entries.push(current)
    }
    return entries.sort()
}

async function recordPath(root, identifier) {
    const matches = (await filesUnder(root)).filter(
        (file) =>
            path.basename(file).startsWith(identifier + '.') &&
            !file.endsWith('.fylo-attrs') &&
            file.split(path.sep).includes('docs')
    )
    expect(matches.length).toBe(1)
    return matches[0]
}

async function pairSnapshot(payload) {
    const metadata = await stat(payload)
    const manifest = await stat(payload + '.fylo-attrs')
    return {
        payloadHash: createHash('sha256')
            .update(await readFile(payload))
            .digest('hex'),
        manifest: JSON.parse(await readFile(payload + '.fylo-attrs', 'utf8')),
        mode: metadata.mode & 0o777,
        uid: metadata.uid,
        gid: metadata.gid,
        mtimeMs: Math.trunc(metadata.mtimeMs),
        manifestMode: manifest.mode & 0o777,
        manifestUid: manifest.uid,
        manifestGid: manifest.gid
    }
}

async function seed(root) {
    const database = new ProfileLoop(root)
    try {
        await database.result({ op: 'handshake' })
        await database.result({ op: 'createCollection', collection: 'records', kind: 'document' })
        await database.result({ op: 'createCollection', collection: 'assets', kind: 'file' })
        await database.result({
            op: 'putData',
            collection: 'records',
            id,
            data: { name: 'Ada', score: 42 },
            meta: { reviewer: 'before', nested: [1, true] },
            access: { mode: 0o640 }
        })
        const input = path.join(os.tmpdir(), path.basename(root) + '-input.bin')
        roots.push(input)
        await writeFile(input, new Uint8Array([0, 1, 2, 255]))
        await database.result({
            op: 'putData',
            collection: 'assets',
            id: rawId,
            file: { path: input, key: '/seed.bin' },
            meta: { source: 'seed' },
            access: { mode: 0o640 }
        })
    } finally {
        await database.close()
    }
}

afterAll(async () => {
    await Promise.all([...loops].map((loop) => loop.close()))
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe.skipIf(!required || process.platform === 'win32')(
    'explicit EFS metadata candidate',
    () => {
        test('retains coupled document/raw metadata through moves and repeated recovery', async () => {
            const root = await freshRoot()
            await seed(root)
            const beforePath = await recordPath(root, id)
            const before = await pairSnapshot(beforePath)
            expect(before.manifestMode).toBe(0o600)
            expect(before.manifestUid).toBe(before.uid)
            expect(before.manifestGid).toBe(before.gid)
            const owner = { uid: before.uid, groups: [before.gid] }
            const database = new ProfileLoop(root)
            try {
                await database.result({ op: 'handshake' })
                const rawBefore = await database.result({
                    op: 'getMeta',
                    collection: 'assets',
                    id: rawId,
                    access: owner
                })
                await database.result({ op: 'delDoc', collection: 'records', id, access: owner })
                expect(await stat(beforePath).catch(() => null)).toBeNull()
                expect(await stat(beforePath + '.fylo-attrs').catch(() => null)).toBeNull()
                const retained = (await filesUnder(root)).find(
                    (file) =>
                        file.endsWith(id + '.json') && file.split(path.sep).includes('.deleted')
                )
                expect(await pairSnapshot(retained)).toEqual(before)
                await database.result({
                    op: 'restoreDoc',
                    collection: 'records',
                    id,
                    access: owner
                })
                expect(await stat(retained + '.fylo-attrs').catch(() => null)).toBeNull()
                expect(await pairSnapshot(await recordPath(root, id))).toEqual(before)
                await database.result({
                    op: 'executeSQL',
                    sql: "DELETE FROM records WHERE name = 'Ada'",
                    access: owner
                })
                await database.result({
                    op: 'restoreDoc',
                    collection: 'records',
                    id,
                    access: owner
                })
                await database.result({ op: 'reshardCollection', collection: 'records', width: 3 })
                expect(await pairSnapshot(await recordPath(root, id))).toEqual(before)
                await database.result({
                    op: 'delDoc',
                    collection: 'assets',
                    id: rawId,
                    access: owner
                })
                await database.result({
                    op: 'restoreDoc',
                    collection: 'assets',
                    id: rawId,
                    access: owner
                })
                await database.result({ op: 'reshardCollection', collection: 'assets', width: 3 })
                const rawAfter = await database.result({
                    op: 'getMeta',
                    collection: 'assets',
                    id: rawId,
                    access: owner
                })
                expect(rawAfter).toEqual(rawBefore)
                const inspection = await database.result({
                    op: 'inspectCollection',
                    collection: 'records'
                })
                expect(Number(inspection.docsStored)).toBe(1)
                expect(Number(inspection.indexedDocs)).toBe(1)
            } finally {
                await database.close()
            }
            for (let pass = 0; pass < 2; pass++) {
                const reopened = new ProfileLoop(root)
                try {
                    await reopened.result({ op: 'handshake' })
                    expect(
                        await reopened.result({
                            op: 'getDoc',
                            collection: 'records',
                            id,
                            access: owner
                        })
                    ).toEqual({ [id]: { name: 'Ada', score: 42 } })
                    expect(await pairSnapshot(await recordPath(root, id))).toEqual(before)
                } finally {
                    await reopened.close()
                }
            }
        })

        test('metadata updates keep payload time and deny access without metadata leakage', async () => {
            const root = await freshRoot()
            await seed(root)
            const payload = await recordPath(root, id)
            const before = await pairSnapshot(payload)
            const owner = { uid: before.uid, groups: [before.gid] }
            const database = new ProfileLoop(root)
            try {
                await database.result({ op: 'handshake' })
                const first = await database.result({
                    op: 'setMeta',
                    collection: 'records',
                    id,
                    meta: { reviewer: 'after', nested: null },
                    access: owner
                })
                expect(first.reviewer).toBe('after')
                expect(first.nested).toBeUndefined()
                const after = await pairSnapshot(payload)
                expect(after.payloadHash).toBe(before.payloadHash)
                expect(after.mtimeMs).toBe(before.mtimeMs)
                expect(after.mode).toBe(before.mode)
                expect(after.manifestMode).toBe(0o600)
                const anonymous = await database.request({
                    op: 'setMeta',
                    collection: 'records',
                    id,
                    meta: { reviewer: 'anonymous' }
                })
                expect(anonymous).toMatchObject({ ok: false, error: { code: 'EACCES' } })
                expect(
                    await database.result({
                        op: 'getMeta',
                        collection: 'records',
                        id,
                        access: owner
                    })
                ).toEqual(first)
                const foreign = (before.uid + 1) >>> 0
                for (const op of ['getDoc', 'getMeta']) {
                    expect(await database.request({ op, collection: 'records', id })).toMatchObject(
                        { ok: false, error: { code: 'EACCES' } }
                    )
                    expect(
                        await database.request({
                            op,
                            collection: 'records',
                            id,
                            access: { uid: foreign, groups: [] }
                        })
                    ).toMatchObject({ ok: false, error: { code: 'EACCES' } })
                }
                const denied = await database.request({
                    op: 'putData',
                    collection: 'records',
                    id: '4VRNF52JPCQ',
                    data: { name: 'foreign' },
                    access: { uid: foreign, mode: 0o600 }
                })
                expect(denied.ok).toBe(false)
                expect(
                    await database.result({
                        op: 'getMeta',
                        collection: 'records',
                        id,
                        access: owner
                    })
                ).toEqual(first)
            } finally {
                await database.close()
            }
        })

        test('commit, branch materialization and restore preserve metadata and permissions', async () => {
            const root = await freshRoot()
            await seed(root)
            const before = await pairSnapshot(await recordPath(root, id))
            const owner = { uid: before.uid, groups: [before.gid] }
            const database = new ProfileLoop(root)
            try {
                await database.result({ op: 'handshake' })
                const original = await database.result({
                    op: 'getMeta',
                    collection: 'records',
                    id,
                    access: owner
                })
                const first = await database.result({
                    op: 'commit',
                    message: 'before metadata change'
                })
                expect(first.id).toBeString()
                const firstHistory = await database.result({ op: 'log', limit: 10 })
                expect(Array.isArray(firstHistory)).toBe(true)
                expect(firstHistory.length).toBeGreaterThan(0)
                expect(firstHistory.length).toBeLessThanOrEqual(10)
                expect(firstHistory[0]).toMatchObject({
                    id: first.id,
                    message: 'before metadata change'
                })
                const branch = await database.result({
                    op: 'checkout',
                    branch: 'review',
                    create: true
                })
                expect(await pairSnapshot(await recordPath(branch.root, id))).toEqual(before)
                await database.result({
                    op: 'setMeta',
                    collection: 'records',
                    id,
                    meta: { reviewer: 'branch' },
                    access: owner
                })
                const changed = await database.result({
                    op: 'commit',
                    message: 'branch metadata change'
                })
                expect(changed.id).not.toBe(first.id)
                await database.result({ op: 'restoreCommit', id: first.id, force: true })
                const restoredMetadata = await database.result({
                    op: 'getMeta',
                    collection: 'records',
                    id,
                    access: owner
                })
                const {
                    updatedAt: originalTime,
                    mtime: originalMtime,
                    ...originalStable
                } = original
                const {
                    updatedAt: restoredTime,
                    mtime: restoredMtime,
                    ...restoredStable
                } = restoredMetadata
                expect(restoredStable).toEqual(originalStable)
                // Version-2 VCS preserves attributes, while payload time reflects
                // materialization. Whole-root copies and rollback retain mtime.
                expect(restoredTime).toBe(restoredMtime)
                expect(originalTime).toBe(originalMtime)
                const restored = await pairSnapshot(await recordPath(branch.root, id))
                expect(restored.manifest).toEqual(before.manifest)
                expect(restored.mode).toBe(before.mode)
                expect(restored.uid).toBe(before.uid)
                expect(restored.gid).toBe(before.gid)
                const inspection = await database.result({
                    op: 'inspectCollection',
                    collection: 'records'
                })
                expect(Number(inspection.docsStored)).toBe(1)
                const history = await database.result({ op: 'log', limit: 10 })
                expect(Array.isArray(history)).toBe(true)
                expect(history.length).toBe(firstHistory.length)
                expect(history).toEqual(firstHistory)
                expect(await database.result({ op: 'log', limit: 1 })).toEqual([firstHistory[0]])
            } finally {
                await database.close()
            }
        })

        test('quiesced complete copy reopens with equivalent queries and permissions', async () => {
            const root = await freshRoot()
            await seed(root)
            const before = await pairSnapshot(await recordPath(root, id))
            const owner = { uid: before.uid, groups: [before.gid] }
            const restored = await freshRoot()
            await cp(root, restored, { recursive: true, preserveTimestamps: true })
            const database = new ProfileLoop(restored)
            try {
                await database.result({ op: 'handshake' })
                expect(
                    await database.result({
                        op: 'getDoc',
                        collection: 'records',
                        id,
                        access: owner
                    })
                ).toEqual({ [id]: { name: 'Ada', score: 42 } })
                expect(await pairSnapshot(await recordPath(restored, id))).toEqual(before)
            } finally {
                await database.close()
            }
        })
    }
)
