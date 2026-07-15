import { describe, expect, test } from 'bun:test'
import { readdir, rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { VersionRepository } from '../../src/versioning/repository.js'
import { createTestRoot } from '../helpers/root.js'

const worker = path.join(import.meta.dir, 'vcs-materialization.worker.js')
const initWorker = path.join(import.meta.dir, 'vcs-init.worker.js')

/** @param {string} root @param {string} commitId @param {string} phase */
async function pauseAtPhase(root, commitId, phase) {
    const marker = path.join(root, '.vcs-kill-ready')
    const child = Bun.spawn(['bun', worker, root, commitId, phase], {
        stdout: 'ignore',
        stderr: 'pipe'
    })
    for (let attempt = 0; attempt < 250 && !(await Bun.file(marker).exists()); attempt++) {
        await Bun.sleep(10)
    }
    if (!(await Bun.file(marker).exists())) {
        child.kill('SIGKILL')
        const stderr = await new Response(child.stderr).text()
        await child.exited
        throw new Error(`VCS worker did not reach ${phase}: ${stderr}`)
    }
    return { child, marker }
}

/** @param {string} root @param {string} commitId @param {string} phase */
async function killAtPhase(root, commitId, phase) {
    const { child, marker } = await pauseAtPhase(root, commitId, phase)
    child.kill('SIGKILL')
    await child.exited
    await rm(marker, { force: true })
}

/** @param {'backup-moved' | 'installed'} phase */
async function recoveryScenario(phase) {
    const root = await createTestRoot(`fylo-vcs-recovery-${phase}-`)
    try {
        const db = new Fylo(root)
        const repository = new VersionRepository(root)
        await db.notes.create()
        const id = await db.notes.put({ version: 'old' })
        const [oldCommit] = await repository.log({ limit: 1 })
        await db.notes.patch(id, { version: 'new' })
        const beforeRef = await repository.readRef('main')

        await killAtPhase(root, oldCommit.id, phase)
        const recovered = new VersionRepository(root)
        await recovered.init()

        const reopened = new Fylo(root, { versioning: { autoCommit: false } })
        const document = (await reopened.notes.get(id).once())[id]
        const ref = await recovered.readRef('main')
        const staging = await readdir(path.join(root, '.fylo-vcs', 'staging')).catch(() => [])
        return { document, ref, oldCommit, beforeRef, staging }
    } finally {
        await rm(root, { recursive: true, force: true })
    }
}

describe('version materialization crash recovery', () => {
    test('startup rolls back a crash after backup and before install', async () => {
        const result = await recoveryScenario('backup-moved')
        expect(result.document.version).toBe('new')
        expect(result.ref.head).toBe(result.beforeRef.head)
        expect(result.staging).toEqual([])
    })

    test('startup rolls forward a crash after install and before ref update', async () => {
        const result = await recoveryScenario('installed')
        expect(result.document.version).toBe('old')
        expect(result.ref.head).toBe(result.oldCommit.id)
        expect(result.staging).toEqual([])
    })

    test('fast-forward and three-way merges use the recoverable materialization path', async () => {
        const root = await createTestRoot('fylo-vcs-merge-')
        try {
            const repository = new VersionRepository(root)
            const main = new Fylo(root)
            await main.notes.create()
            const left = await main.notes.put({ value: 'left-v1' })
            const right = await main.notes.put({ value: 'right-v1' })

            await repository.checkout('fast-forward', { create: true })
            const fastForward = new Fylo(root)
            await fastForward.notes.patch(right, { value: 'right-v2' })
            const fastForwardHead = (await repository.readRef('fast-forward')).head
            await repository.checkout('main')
            const forwarded = await repository.merge('fast-forward')
            expect(forwarded.mode).toBe('fast-forward')
            expect((await repository.readRef('main')).head).toBe(fastForwardHead)

            await repository.checkout('feature', { create: true })
            const feature = new Fylo(root)
            await feature.notes.patch(right, { value: 'right-v3' })
            await repository.checkout('main')
            await main.notes.patch(left, { value: 'left-v2' })
            const merged = await repository.merge('feature')

            expect(merged.mode).toBe('merge')
            expect(merged.merged).toBe(true)
            const reopened = new Fylo(root, { versioning: { autoCommit: false } })
            expect((await reopened.notes.get(left).once())[left].value).toBe('left-v2')
            expect((await reopened.notes.get(right).once())[right].value).toBe('right-v3')
            expect((await repository.status()).clean).toBe(true)
        } finally {
            await rm(root, { recursive: true, force: true })
        }
    })

    test('storage writes wait for materialization and recover a killed owner before proceeding', async () => {
        const root = await createTestRoot('fylo-vcs-write-serialization-')
        try {
            const db = new Fylo(root)
            const repository = new VersionRepository(root)
            await db.notes.create()
            const id = await db.notes.put({ version: 'old' })
            const [oldCommit] = await repository.log({ limit: 1 })
            await db.notes.patch(id, { version: 'new' })

            const { child, marker } = await pauseAtPhase(root, oldCommit.id, 'backup-moved')
            const concurrent = new Fylo(root)
            let settled = false
            const write = concurrent.notes.put({ concurrent: true }).finally(() => {
                settled = true
            })
            await Bun.sleep(50)
            expect(settled).toBe(false)

            child.kill('SIGKILL')
            await child.exited
            await rm(marker, { force: true })
            const concurrentId = await write
            expect((await concurrent.notes.get(id).once())[id].version).toBe('new')
            expect((await concurrent.notes.get(concurrentId).once())[concurrentId].concurrent).toBe(
                true
            )
        } finally {
            await rm(root, { recursive: true, force: true })
        }
    })

    test('concurrent repository startup recovers one stranded transaction idempotently', async () => {
        const root = await createTestRoot('fylo-vcs-init-stampede-')
        try {
            const db = new Fylo(root)
            const repository = new VersionRepository(root)
            await db.notes.create()
            const id = await db.notes.put({ version: 'old' })
            const [oldCommit] = await repository.log({ limit: 1 })
            await db.notes.patch(id, { version: 'new' })
            const beforeRef = await repository.readRef('main')

            const { child, marker } = await pauseAtPhase(root, oldCommit.id, 'backup-moved')
            child.kill('SIGKILL')
            await child.exited
            await rm(marker, { force: true })

            const workers = Array.from({ length: 20 }, () =>
                Bun.spawn(['bun', initWorker, root], {
                    stdout: 'ignore',
                    stderr: 'pipe'
                })
            )
            for (const startup of workers) {
                const [exitCode, stderr] = await Promise.all([
                    startup.exited,
                    new Response(startup.stderr).text()
                ])
                expect(stderr).toBe('')
                expect(exitCode).toBe(0)
            }

            // Repeated in-process initialization is also idempotent after the
            // recovery winner removes the transaction directory.
            await Promise.all(Array.from({ length: 20 }, () => new VersionRepository(root).init()))
            const recovered = new VersionRepository(root)
            expect((await recovered.readRef('main')).head).toBe(beforeRef.head)
            const reopened = new Fylo(root, { versioning: { autoCommit: false } })
            expect((await reopened.notes.get(id).once())[id].version).toBe('new')
            expect(await readdir(path.join(root, '.fylo-vcs', 'staging'))).toEqual([])
        } finally {
            await rm(root, { recursive: true, force: true })
        }
    })
})
