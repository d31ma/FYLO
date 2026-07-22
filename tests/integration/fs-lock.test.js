import { afterAll, describe, expect, test } from 'bun:test'
import { mkdtemp, readdir, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import TTID from '../helpers/ttid.js'
import { tryAcquireFileLock, tryReleaseFileLock } from '../../src/storage/fs-lock.js'
import { FilesystemLockManager, FilesystemStorage } from '../../src/storage/primitives.js'

const root = await mkdtemp(path.join(os.tmpdir(), 'fylo-fslock-'))
const CONTENDER_WORKER = path.join(import.meta.dir, 'fs-lock-contender.worker.js')
const CLAIM_HOLDER_WORKER = path.join(import.meta.dir, 'fs-lock-claim-holder.worker.js')

describe('tryAcquireFileLock / tryReleaseFileLock', () => {
    afterAll(async () => {
        await rm(root, { recursive: true, force: true })
    })
    test('single acquire succeeds', async () => {
        const lock = path.join(root, 'a.lock')
        expect(await tryAcquireFileLock(lock, 'owner-1')).toBe(true)
        await tryReleaseFileLock(lock, 'owner-1')
    })
    test('concurrent acquires: exactly one wins', async () => {
        const lock = path.join(root, 'concurrent.lock')
        const results = await Promise.all(
            Array.from({ length: 8 }, (_, i) => tryAcquireFileLock(lock, `owner-${i}`))
        )
        const winners = results.filter(Boolean)
        expect(winners).toHaveLength(1)
        await tryReleaseFileLock(lock, 'owner-0')
        await tryReleaseFileLock(lock, 'owner-1')
        await tryReleaseFileLock(lock, 'owner-2')
        await tryReleaseFileLock(lock, 'owner-3')
        await tryReleaseFileLock(lock, 'owner-4')
        await tryReleaseFileLock(lock, 'owner-5')
        await tryReleaseFileLock(lock, 'owner-6')
        await tryReleaseFileLock(lock, 'owner-7')
    })
    test('second acquire fails while first is live', async () => {
        const lock = path.join(root, 'live.lock')
        expect(await tryAcquireFileLock(lock, 'A', 60_000)).toBe(true)
        expect(await tryAcquireFileLock(lock, 'B', 60_000)).toBe(false)
        await tryReleaseFileLock(lock, 'A')
    })
    test('a live process lock is not taken over merely because its TTL expires', async () => {
        const lock = path.join(root, 'stale.lock')
        expect(await tryAcquireFileLock(lock, 'A', 1)).toBe(true)
        await Bun.sleep(10)
        expect(await tryAcquireFileLock(lock, 'B', 1)).toBe(false)
        await tryReleaseFileLock(lock, 'A')
    })
    test('a stale legacy lock without a pid is reclaimed after TTL expires', async () => {
        const lock = path.join(root, 'legacy-stale.lock')
        await Bun.write(lock, JSON.stringify({ owner: 'old', ts: Date.now() - 10_000 }))
        expect(await tryAcquireFileLock(lock, 'B', 1)).toBe(true)
        await tryReleaseFileLock(lock, 'B')
    })
    test('multiprocess stale takeover elects exactly one winner', async () => {
        const directory = await mkdtemp(path.join(root, 'takeover-'))
        const lock = path.join(directory, 'contended.lock')
        const release = path.join(directory, 'release')
        await writeFile(lock, JSON.stringify({ owner: 'stale', ts: 0 }))
        const workers = Array.from({ length: 12 }, (_, index) =>
            Bun.spawn(['bun', CONTENDER_WORKER, lock, `owner-${index}`, directory, release], {
                stdout: 'pipe',
                stderr: 'pipe'
            })
        )
        const deadline = Date.now() + 10_000
        let results = []
        while (Date.now() < deadline) {
            results = (await readdir(directory)).filter((entry) => entry.endsWith('.result'))
            if (results.length === workers.length) break
            await Bun.sleep(10)
        }
        expect(results).toHaveLength(workers.length)
        const outcomes = await Promise.all(
            results.map(async (entry) =>
                JSON.parse(await Bun.file(path.join(directory, entry)).text())
            )
        )
        expect(outcomes.filter(Boolean)).toHaveLength(1)
        await writeFile(release, '')
        await Promise.all(workers.map((worker) => worker.exited))
    }, 15_000)
    test('does not mistake a reused live PID for the original lock owner', async () => {
        const lock = path.join(root, 'pid-reuse.lock')
        await writeFile(
            lock,
            JSON.stringify({
                owner: 'previous-process',
                pid: process.pid,
                processIdentity: 'a-different-process-incarnation',
                ts: Date.now()
            })
        )
        expect(await tryAcquireFileLock(lock, 'replacement')).toBe(true)
        await tryReleaseFileLock(lock, 'replacement')
    })
    test('a killed takeover contender cannot strand the takeover claim', async () => {
        const directory = await mkdtemp(path.join(root, 'dead-claim-'))
        const lock = path.join(directory, 'stale.lock')
        const ready = path.join(directory, 'ready')
        await writeFile(lock, JSON.stringify({ owner: 'stale', ts: 0 }))
        const holder = Bun.spawn(['bun', CLAIM_HOLDER_WORKER, `${lock}.takeover`, ready], {
            stdout: 'pipe',
            stderr: 'pipe'
        })
        const deadline = Date.now() + 5_000
        while (!(await Bun.file(ready).exists()) && Date.now() < deadline) await Bun.sleep(5)
        expect(await Bun.file(ready).exists()).toBe(true)
        expect(await tryAcquireFileLock(lock, 'blocked', 1)).toBe(false)
        holder.kill('SIGKILL')
        await holder.exited

        expect(await Bun.file(`${lock}.takeover`).exists()).toBe(true)
        expect(await tryAcquireFileLock(lock, 'recovered', 1)).toBe(true)
        await tryReleaseFileLock(lock, 'recovered')
    })
    test('release with wrong owner is a no-op', async () => {
        const lock = path.join(root, 'owner-check.lock')
        expect(await tryAcquireFileLock(lock, 'A', 60_000)).toBe(true)
        await tryReleaseFileLock(lock, 'B')
        expect(await tryAcquireFileLock(lock, 'C', 60_000)).toBe(false)
        await tryReleaseFileLock(lock, 'A')
    })
    test('release of missing lock file is a no-op', async () => {
        const lock = path.join(root, 'missing.lock')
        await tryReleaseFileLock(lock, 'nobody')
    })
    test('corrupt lock payload is treated as stale', async () => {
        const lock = path.join(root, 'corrupt.lock')
        await Bun.write(lock, 'not json{')
        expect(await tryAcquireFileLock(lock, 'A', 60_000)).toBe(true)
        await tryReleaseFileLock(lock, 'A')
    })
    test('can re-acquire after explicit release', async () => {
        const lock = path.join(root, 'reacquire.lock')
        expect(await tryAcquireFileLock(lock, 'A', 60_000)).toBe(true)
        await tryReleaseFileLock(lock, 'A')
        expect(await tryAcquireFileLock(lock, 'B', 60_000)).toBe(true)
        await tryReleaseFileLock(lock, 'B')
    })
    test('heartbeat keeps a long-held lock from being taken over', async () => {
        const lock = path.join(root, 'heartbeat.lock')
        expect(await tryAcquireFileLock(lock, 'A', { ttlMs: 100, heartbeat: true })).toBe(true)
        await Bun.sleep(350)
        expect(await tryAcquireFileLock(lock, 'B', { ttlMs: 100 })).toBe(false)
        await tryReleaseFileLock(lock, 'A')
        expect(await tryAcquireFileLock(lock, 'C', { ttlMs: 100 })).toBe(true)
        await tryReleaseFileLock(lock, 'C')
    })
    test('release drains heartbeat — no ghost lock after release', async () => {
        const lock = path.join(root, 'heartbeat-release.lock')
        for (let i = 0; i < 5; i++) {
            expect(await tryAcquireFileLock(lock, 'A', { ttlMs: 300, heartbeat: true })).toBe(true)
            await Bun.sleep(110)
            await tryReleaseFileLock(lock, 'A')
            expect(await Bun.file(lock).exists()).toBe(false)
        }
    })
})

describe('FilesystemLockManager', () => {
    test('acquire/release with valid TTID-format docId', async () => {
        const collection = 'lm-test'
        const storage = new FilesystemStorage()
        const manager = new FilesystemLockManager(
            (name) => path.join(root, '.collections', name),
            storage
        )
        const docId = TTID.generate()
        expect(await manager.acquire(collection, docId, 'owner-A', 60_000)).toBe(true)
        expect(await manager.acquire(collection, docId, 'owner-B', 60_000)).toBe(false)
        await manager.release(collection, docId, 'owner-B')
        expect(await manager.acquire(collection, docId, 'owner-C', 60_000)).toBe(false)
        await manager.release(collection, docId, 'owner-A')
        expect(await manager.acquire(collection, docId, 'owner-D', 60_000)).toBe(true)
        await manager.release(collection, docId, 'owner-D')
    })
})
