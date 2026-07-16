import { afterAll, describe, expect, test, spyOn } from 'bun:test'
import { rm, stat, writeFile } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { removeXattr, setXattr } from '../../src/storage/xattr.js'
import { CHECKSUM_XATTR, FilesystemFiles, KEY_XATTR } from '../../src/storage/files.js'
import { VersionRepository } from '../../src/versioning/repository.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-drive-')
const fylo = new Fylo(root, { versioning: { autoCommit: false } })
await fylo.drive.create({ kind: 'file' })

afterAll(async () => {
    await rm(root, { recursive: true, force: true })
})

describe('drive-style file collections', () => {
    test('checksum is cached: listings do not re-hash file contents', async () => {
        const id = await fylo.drive.put(new File(['cache me'], 'cache.txt'), {
            key: '/cache/cache.txt'
        })
        const first = (await fylo.drive.get(id).once())[id]
        const hashSpy = spyOn(FilesystemFiles.prototype, 'hash')
        const again = (await fylo.drive.get(id).once())[id]
        expect(hashSpy).toHaveBeenCalledTimes(0)
        expect(again.checksumSHA256).toBe(first.checksumSHA256)
        hashSpy.mockRestore()
    })

    test('checksum cache survives soft delete and restore without re-hashing', async () => {
        const id = await fylo.drive.put(new File(['mover'], 'mover.txt'), {
            key: '/cache/mover.txt'
        })
        await fylo.drive.delete(id)
        const hashSpy = spyOn(FilesystemFiles.prototype, 'hash')
        const deleted = []
        for await (const entry of fylo.drive.find.deleted({}).collect()) deleted.push(entry)
        expect(deleted.some((entry) => entry[id])).toBe(true)
        await fylo.drive.restore(id)
        await fylo.drive.get(id).once()
        expect(hashSpy).toHaveBeenCalledTimes(0)
        hashSpy.mockRestore()
    })

    test('folder() lists one level: child files and immediate subfolders', async () => {
        await fylo.tree.create({ kind: 'file' })
        const top = await fylo.tree.put(new File(['t'], 't.txt'), { key: '/top.txt' })
        await fylo.tree.put(new File(['a'], 'a.txt'), { key: '/photos/2026/a.jpg' })
        await fylo.tree.put(new File(['b'], 'b.txt'), { key: '/photos/b.jpg' })

        const rootLevel = await fylo.tree.folder('/')
        expect(Object.keys(rootLevel.files)).toEqual([top])
        expect(rootLevel.folders).toEqual(['photos'])

        const photos = await fylo.tree.folder('/photos/')
        expect(Object.values(photos.files).map((f) => f.key)).toEqual(['/photos/b.jpg'])
        expect(photos.folders).toEqual(['2026'])

        await expect(fylo.tree.folder('nope')).rejects.toThrow("start and end with '/'")
    })

    test('rekey moves a file to a new key in place and reindexes it', async () => {
        const id = await fylo.drive.put(new File(['payload'], 'r.txt'), { key: '/inbox/r.txt' })
        const before = (await fylo.drive.get(id).once())[id]

        expect(await fylo.drive.rekey(id, '/archive/r.txt')).toBe('/archive/r.txt')
        const after = (await fylo.drive.get(id).once())[id]
        expect(after.key).toBe('/archive/r.txt')
        expect(after.checksumSHA256).toBe(before.checksumSHA256)

        const matches = await Array.fromAsync(
            fylo.drive.find({ $ops: [{ key: { $eq: '/archive/r.txt' } }] }).collect()
        )
        expect(matches.some((entry) => entry[id])).toBe(true)
        const stale = await Array.fromAsync(
            fylo.drive.find({ $ops: [{ key: { $eq: '/inbox/r.txt' } }] }).collect()
        )
        expect(stale.some((entry) => entry[id])).toBe(false)
    })

    test('rekey rejects a key that another active file holds', async () => {
        const one = await fylo.drive.put(new File(['1'], 'one.txt'), { key: '/conflict/one.txt' })
        await fylo.drive.put(new File(['2'], 'two.txt'), { key: '/conflict/two.txt' })
        await expect(fylo.drive.rekey(one, '/conflict/two.txt')).rejects.toThrow(
            'Object key already exists'
        )
    })

    test('rekey restores the key and indexes when index refresh fails', async () => {
        const id = await fylo.drive.put(new File(['atomic'], 'atomic.txt'), {
            key: '/atomic/original.txt'
        })
        const index = fylo.engine.index
        const originalPut = index.putDocument
        let injected = false
        index.putDocument = /** @type {typeof index.putDocument} */ (
            async function (collection, docId, doc) {
                if (!injected) {
                    injected = true
                    throw new Error('injected index refresh failure')
                }
                return await originalPut.call(this, collection, docId, doc)
            }
        )
        try {
            await expect(fylo.drive.rekey(id, '/atomic/changed.txt')).rejects.toThrow(
                'injected index refresh failure'
            )
        } finally {
            index.putDocument = originalPut
        }

        expect((await fylo.drive.get(id).once())[id].key).toBe('/atomic/original.txt')
        const oldHits = await Array.fromAsync(
            fylo.drive.find({ $ops: [{ key: { $eq: '/atomic/original.txt' } }] }).collect()
        )
        const newHits = await Array.fromAsync(
            fylo.drive.find({ $ops: [{ key: { $eq: '/atomic/changed.txt' } }] }).collect()
        )
        expect(oldHits.some((entry) => entry[id])).toBe(true)
        expect(newHits.some((entry) => entry[id])).toBe(false)
    })

    test('rekey.prefix moves a whole folder without rewriting bytes', async () => {
        await fylo.moves.create({ kind: 'file' })
        const a = await fylo.moves.put(new File(['a'], 'a.txt'), { key: '/old/a.txt' })
        const b = await fylo.moves.put(new File(['b'], 'b.txt'), { key: '/old/deep/b.txt' })
        const other = await fylo.moves.put(new File(['c'], 'c.txt'), { key: '/keep/c.txt' })

        expect(await fylo.moves.rekey.prefix('/old/', '/new/')).toBe(2)
        expect((await fylo.moves.get(a).once())[a].key).toBe('/new/a.txt')
        expect((await fylo.moves.get(b).once())[b].key).toBe('/new/deep/b.txt')
        expect((await fylo.moves.get(other).once())[other].key).toBe('/keep/c.txt')
    })

    test('meta is set at put(), returned in manifests, and queryable through find()', async () => {
        const starred = await fylo.drive.put(new File(['s'], 's.txt'), {
            key: '/meta/s.txt',
            meta: { starred: true, rating: 5 }
        })
        const plain = await fylo.drive.put(new File(['p'], 'p.txt'), { key: '/meta/p.txt' })

        expect((await fylo.drive.get(starred).once())[starred].meta).toEqual({
            starred: true,
            rating: 5
        })
        expect((await fylo.drive.get(plain).once())[plain].meta).toBeUndefined()

        const hits = await Array.fromAsync(
            fylo.drive.find({ $ops: [{ ['meta/starred']: { $eq: true } }] }).collect()
        )
        expect(hits.some((entry) => entry[starred])).toBe(true)
        expect(hits.some((entry) => entry[plain])).toBe(false)

        const highRated = await Array.fromAsync(
            fylo.drive.find({ $ops: [{ ['meta/rating']: { $gte: 4 } }] }).collect()
        )
        expect(highRated.some((entry) => entry[starred])).toBe(true)

        await fylo.drive.put(starred).metadata({ starred: null })
        const afterRemove = await Array.fromAsync(
            fylo.drive.find({ $ops: [{ ['meta/starred']: { $eq: true } }] }).collect()
        )
        expect(afterRemove.some((entry) => entry[starred])).toBe(false)
        expect(await fylo.drive.get(starred).metadata()).toMatchObject({ rating: 5 })
    })

    test('file metadata restores xattrs and indexes when index refresh fails', async () => {
        const id = await fylo.drive.put(new File(['atomic meta'], 'meta.txt'), {
            key: '/atomic/meta.txt',
            meta: { stage: 'original' }
        })
        const beforeTimestamp = await fylo.engine.docMetaUpdatedAt('drive', id)
        const index = fylo.engine.index
        const originalPut = index.putDocument
        let injected = false
        index.putDocument = /** @type {typeof index.putDocument} */ (
            async function (collection, docId, doc) {
                if (!injected) {
                    injected = true
                    throw new Error('injected metadata index failure')
                }
                return await originalPut.call(this, collection, docId, doc)
            }
        )
        try {
            await expect(fylo.drive.put(id).metadata({ stage: 'changed' })).rejects.toThrow(
                'injected metadata index failure'
            )
        } finally {
            index.putDocument = originalPut
        }

        expect(await fylo.drive.get(id).metadata()).toMatchObject({ stage: 'original' })
        expect(await fylo.engine.docMetaUpdatedAt('drive', id)).toBe(beforeTimestamp)
        const oldHits = await Array.fromAsync(
            fylo.drive.find({ $ops: [{ ['meta/stage']: { $eq: 'original' } }] }).collect()
        )
        const newHits = await Array.fromAsync(
            fylo.drive.find({ $ops: [{ ['meta/stage']: { $eq: 'changed' } }] }).collect()
        )
        expect(oldHits.some((entry) => entry[id])).toBe(true)
        expect(newHits.some((entry) => entry[id])).toBe(false)
    })

    test('collections created with versioned:false stay out of history and survive restores', async () => {
        const vcsRoot = await createTestRoot('fylo-drive-vcs-')
        try {
            const db = new Fylo(vcsRoot)
            const repo = new VersionRepository(vcsRoot)
            await db.notes.create()
            await db.blobs.create({ kind: 'file', versioned: false })

            const noteId = await db.notes.put({ title: 'versioned' })
            const blobId = await db.blobs.put(new File(['big media'], 'big.bin'), {
                key: '/big.bin'
            })

            expect((await repo.status()).clean).toBe(true)
            const commits = await repo.log()
            expect(commits.some((c) => c.message.includes('blobs'))).toBe(false)
            expect(commits.some((c) => c.message.includes('notes'))).toBe(true)

            // Restoring history must not wipe the unversioned working files.
            await db.notes.delete(noteId)
            const target = commits.find((c) => c.message === `put notes/${noteId}`)
            await repo.restoreCommit(target.id)
            const restored = new Fylo(vcsRoot)
            expect((await restored.notes.get(noteId).once())[noteId].title).toBe('versioned')
            expect(new TextDecoder().decode(await restored.blobs.get(blobId).bytes())).toBe(
                'big media'
            )
        } finally {
            await rm(vcsRoot, { recursive: true, force: true })
        }
    })

    test('verify() catches a byte-tamper that the checksum stamp cannot see', async () => {
        await fylo.audit.create({ kind: 'file' })
        const good = await fylo.audit.put(new File(['untouched'], 'good.txt'), {
            key: '/good.txt'
        })
        const bad = await fylo.audit.put(new File(['original!'], 'bad.txt'), { key: '/bad.txt' })
        const claimed = (await fylo.audit.get(bad).once())[bad].checksumSHA256

        // Tamper: same byte length, then forge the stamp to match the new
        // size/mtime while still claiming the original hash — the exact state
        // silent corruption or a stamp-preserving tamper leaves behind.
        const target = (await fylo.engine.files.readStoredFile('audit', bad)).path
        await writeFile(target, 'tampered!')
        const meta = await stat(target)
        setXattr(target, CHECKSUM_XATTR, `${claimed}:${meta.size}:${meta.mtimeMs}`)

        // The fast path is blind to it (this is the documented trade-off)...
        expect((await fylo.audit.get(bad).once())[bad].checksumSHA256).toBe(claimed)

        // ...and the stamp-ignoring audit catches it.
        const events = []
        const auditor = new Fylo(root, {
            versioning: { autoCommit: false },
            onEvent: (event) => events.push(event)
        })
        const report = await auditor.audit.verify()
        expect(report.corrupt).toEqual([
            expect.objectContaining({ id: bad, namespace: 'active', expected: claimed })
        ])
        expect(report.verified).toBeGreaterThanOrEqual(1)
        expect(events.some((e) => e.type === 'file.checksum-mismatch' && e.docId === bad)).toBe(
            true
        )

        // The good file's claim survives; the corrupt file's stamp is untouched
        // so the original claim remains on record.
        expect((await fylo.audit.get(good).once())[good].key).toBe('/good.txt')
        expect((await fylo.audit.get(bad).once())[bad].checksumSHA256).toBe(claimed)
    })

    test('verify() re-stamps files that lost their checksum stamp', async () => {
        const id = await fylo.drive.put(new File(['stampless'], 'st.txt'), {
            key: '/verify/st.txt'
        })
        const target = (await fylo.engine.files.readStoredFile('drive', id)).path
        removeXattr(target, CHECKSUM_XATTR)

        const report = await fylo.drive.verify()
        expect(report.stamped).toBeGreaterThanOrEqual(1)
        expect(report.corrupt).toEqual([])

        // Freshly stamped: subsequent reads use the cache again.
        const hashSpy = spyOn(FilesystemFiles.prototype, 'hash')
        await fylo.drive.get(id).once()
        expect(hashSpy).toHaveBeenCalledTimes(0)
        hashSpy.mockRestore()
    })

    test('fylo verify CLI and machine op report corruption and exit non-zero', async () => {
        const cliRoot = await createTestRoot('fylo-drive-cli-')
        try {
            const db = new Fylo(cliRoot, { versioning: { autoCommit: false } })
            await db.vault.create({ kind: 'file' })
            const id = await db.vault.put(new File(['pristine'], 'v.txt'), { key: '/v.txt' })

            /** @param {string[]} args */
            const runCli = async (args) => {
                const proc = Bun.spawn(['bun', 'src/cli/index.js', ...args], {
                    cwd: process.cwd(),
                    stdout: 'pipe',
                    stderr: 'pipe'
                })
                const [stdout, stderr, exitCode] = await Promise.all([
                    new Response(proc.stdout).text(),
                    new Response(proc.stderr).text(),
                    proc.exited
                ])
                return { stdout, stderr, exitCode }
            }

            const clean = await runCli(['verify', 'vault', '--root', cliRoot, '--json'])
            expect(clean.exitCode).toBe(0)
            expect(JSON.parse(clean.stdout)).toMatchObject({ filesScanned: 1, corrupt: [] })

            // Forge the stamp over tampered bytes, then expect a failing audit.
            const claimed = (await db.vault.get(id).once())[id].checksumSHA256
            const target = (await db.engine.files.readStoredFile('vault', id)).path
            await writeFile(target, 'corrupt!!')
            const meta = await stat(target)
            setXattr(target, CHECKSUM_XATTR, `${claimed}:${meta.size}:${meta.mtimeMs}`)

            const dirty = await runCli(['verify', 'vault', '--root', cliRoot])
            expect(dirty.exitCode).toBe(1)
            expect(dirty.stdout).toContain('Corrupt: 1')
            expect(dirty.stdout).toContain(id)

            const { runMachineRequest } = await import('../../src/cli/machine.js')
            const response = await runMachineRequest(
                { op: 'verifyCollection', collection: 'vault' },
                { root: cliRoot }
            )
            expect(response.ok).toBe(true)
            expect(response.result.corrupt).toHaveLength(1)
        } finally {
            await rm(cliRoot, { recursive: true, force: true })
        }
    })

    test('rebuild() repairs a file whose key xattr was stripped', async () => {
        await fylo.stripped.create({ kind: 'file' })
        const id = await fylo.stripped.put(new File(['bytes'], 'lost.txt'), {
            key: '/custom/lost.txt'
        })
        const storedPath = (await fylo.engine.files.readStoredFile('stripped', id)).path
        removeXattr(storedPath, KEY_XATTR)

        // Fail closed on plain reads...
        await expect(fylo.stripped.get(id).once()).rejects.toThrow('metadata is missing')

        // ...and repair to the degraded default key on rebuild.
        const events = []
        const repaired = new Fylo(root, {
            versioning: { autoCommit: false },
            onEvent: (event) => events.push(event)
        })
        await repaired.stripped.rebuild()
        expect(events.some((e) => e.type === 'file.key-repaired')).toBe(true)
        const manifest = await repaired.stripped.get(id).once()
        expect(manifest[id].key).toBe(`/${manifest[id].name}`)
        expect(new TextDecoder().decode(await repaired.stripped.get(id).bytes())).toBe('bytes')
    })
})
