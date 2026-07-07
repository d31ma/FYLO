import { afterAll, describe, expect, test } from 'bun:test'
import { mkdtemp, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import TTID from '../helpers/ttid.js'
import { tryAcquireFileLock, tryReleaseFileLock } from '../../src/storage/fs-lock.js'
import { FilesystemLockManager, FilesystemStorage } from '../../src/storage/primitives.js'

const root = await mkdtemp(path.join(os.tmpdir(), 'fylo-fslock-'))

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
    test('stale lock is taken over after TTL expires', async () => {
        const lock = path.join(root, 'stale.lock')
        expect(await tryAcquireFileLock(lock, 'A', 1)).toBe(true)
        await Bun.sleep(10)
        expect(await tryAcquireFileLock(lock, 'B', 1)).toBe(true)
        await tryReleaseFileLock(lock, 'B')
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
        const manager = new FilesystemLockManager(root, storage)
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
