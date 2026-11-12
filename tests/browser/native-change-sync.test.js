import { describe, expect, test } from 'bun:test'
import TTID from '../../src/browser/vendor/ttid.mjs'
import { BrowserCore } from '../../src/browser/core/engine.js'
import { createMemoryFilesystem } from '../../src/browser/core/memory-filesystem.js'

class NativeSyncSamples {
    static collection = 'notes'
    static statePath = '/.fylo-transactions/.collections/notes/state.json'
    static logPath = '/.fylo-transactions/.collections/notes/log/segment.log'

    static async core(fs = createMemoryFilesystem()) {
        const fylo = new BrowserCore({ fs, root: '/' })
        await fylo.ready()
        await fylo.createCollection(NativeSyncSamples.collection)
        return { fylo, fs }
    }

    /** @param {any} fs @param {number} generation @param {string} [state] @param {number} [logLength] */
    static async publish(fs, generation, state = 'stable', logLength = undefined) {
        const record = { format: 'fylo.collection-generation.v1', generation, state }
        if (logLength !== undefined) record.log = { length: logLength }
        await fs.mkdir('/.fylo-transactions/.collections/notes', { recursive: true })
        await fs.writeText(NativeSyncSamples.statePath, JSON.stringify(record))
    }

    /** @param {BrowserCore} fylo @param {any} fs @param {string} id @param {Record<string, any>} data */
    static async nativeWrite(fylo, fs, id, data) {
        await Bun.sleep(2)
        const target = fylo.docPath(NativeSyncSamples.collection, id)
        await fs.mkdir(target.slice(0, target.lastIndexOf('/')), { recursive: true })
        await fs.writeText(target, JSON.stringify(data))
    }

    /** @param {BrowserCore} fylo @param {any} fs @param {string} id */
    static async nativeDelete(fylo, fs, id) {
        const source = fylo.docPath(NativeSyncSamples.collection, id)
        const target = fylo.deletedPath(NativeSyncSamples.collection, id)
        await fs.mkdir(target.slice(0, target.lastIndexOf('/')), { recursive: true })
        await fs.move(source, target)
    }

    /** @param {any} fs */
    static async journalLines(fs) {
        const path = '/.collections/notes/events/notes.ndjson'
        if (!(await fs.exists(path))) return []
        return (await fs.readText(path))
            .trim()
            .split('\n')
            .map((line) => JSON.parse(line))
    }

    /** @param {() => Promise<unknown>} action */
    static async errorCode(action) {
        try {
            await action()
        } catch (error) {
            return /** @type {{ code?: string }} */ (error).code
        }
        return 'no error'
    }

    /** @param {{ id: string, kind: string }[]} changes */
    static ids(changes) {
        return changes.map((change) => `${change.kind}:${change.id}`).sort()
    }
}

describe('native change sync', () => {
    test('a root no native engine wrote keeps its journal untouched', async () => {
        const { fylo, fs } = await NativeSyncSamples.core()
        const id = await fylo.putData(NativeSyncSamples.collection, { title: 'one' })
        const page = await fylo.changes(NativeSyncSamples.collection, null)
        expect(page.changes).toEqual([{ id, kind: 'put' }])
        expect(await fs.exists('/.collections/notes/events/notes.native.json')).toBe(false)
        expect(await NativeSyncSamples.journalLines(fs)).toHaveLength(1)
    })

    test('native puts, updates, deletes and restores reach the feed once each', async () => {
        const { fylo, fs } = await NativeSyncSamples.core()
        const own = await fylo.putData(NativeSyncSamples.collection, { title: 'own' })
        const first = TTID.generate()
        const second = TTID.generate()
        await NativeSyncSamples.nativeWrite(fylo, fs, first, { title: 'first' })
        await NativeSyncSamples.nativeWrite(fylo, fs, second, { title: 'second' })
        await NativeSyncSamples.publish(fs, 2)
        const start = await fylo.changes(NativeSyncSamples.collection, null)
        expect(NativeSyncSamples.ids(start.changes)).toEqual(
            NativeSyncSamples.ids([
                { id: own, kind: 'put' },
                { id: first, kind: 'put' },
                { id: second, kind: 'put' }
            ])
        )

        await NativeSyncSamples.nativeWrite(fylo, fs, first, { title: 'first, changed' })
        await NativeSyncSamples.nativeDelete(fylo, fs, second)
        await NativeSyncSamples.nativeDelete(fylo, fs, own)
        await NativeSyncSamples.publish(fs, 4)
        const next = await fylo.changes(NativeSyncSamples.collection, start.cursor)
        expect(NativeSyncSamples.ids(next.changes)).toEqual(
            NativeSyncSamples.ids([
                { id: first, kind: 'put' },
                { id: second, kind: 'delete' },
                { id: own, kind: 'delete' }
            ])
        )

        await fs.move(
            fylo.deletedPath(NativeSyncSamples.collection, second),
            fylo.docPath(NativeSyncSamples.collection, second)
        )
        await NativeSyncSamples.publish(fs, 6)
        const restored = await fylo.changes(NativeSyncSamples.collection, next.cursor)
        expect(restored.changes).toEqual([{ id: second, kind: 'put' }])
        const quiet = await fylo.changes(NativeSyncSamples.collection, restored.cursor)
        expect(quiet.changes).toEqual([])
        expect(await NativeSyncSamples.journalLines(fs)).toHaveLength(7)
    })

    test('a native commit during the scan is neither missed nor doubled', async () => {
        const fs = createMemoryFilesystem()
        const { fylo } = await NativeSyncSamples.core(fs)
        const first = TTID.generate()
        const late = TTID.generate()
        await NativeSyncSamples.nativeWrite(fylo, fs, first, { title: 'first' })
        await NativeSyncSamples.publish(fs, 2)
        const mtimeMs = fs.mtimeMs.bind(fs)
        let commitsDuringScan = 1
        fs.mtimeMs = async (path) => {
            const value = await mtimeMs(path)
            if (commitsDuringScan === 0 || !path.endsWith(`${first}.json`)) return value
            commitsDuringScan -= 1
            await NativeSyncSamples.nativeWrite(fylo, fs, late, { title: 'late' })
            await NativeSyncSamples.publish(fs, 4)
            return value
        }
        const page = await fylo.changes(NativeSyncSamples.collection, null)
        expect(NativeSyncSamples.ids(page.changes)).toEqual(
            NativeSyncSamples.ids([
                { id: first, kind: 'put' },
                { id: late, kind: 'put' }
            ])
        )
        expect(await NativeSyncSamples.journalLines(fs)).toHaveLength(2)
        const record = JSON.parse(await fs.readText('/.collections/notes/events/notes.native.json'))
        expect(record.generation).toBe(4)
    })

    test('a file a native commit moves during the scan is read again, not lost', async () => {
        const fs = createMemoryFilesystem()
        const { fylo } = await NativeSyncSamples.core(fs)
        const first = TTID.generate()
        await NativeSyncSamples.nativeWrite(fylo, fs, first, { title: 'first' })
        await NativeSyncSamples.publish(fs, 2)
        const mtimeMs = fs.mtimeMs.bind(fs)
        let moved = false
        fs.mtimeMs = async (path) => {
            if (moved || !path.endsWith(`${first}.json`)) return await mtimeMs(path)
            moved = true
            await NativeSyncSamples.nativeDelete(fylo, fs, first)
            await NativeSyncSamples.publish(fs, 4)
            throw new Error(`not found: ${path}`)
        }
        const page = await fylo.changes(NativeSyncSamples.collection, null)
        expect(page.changes).toEqual([{ id: first, kind: 'delete' }])
        expect(await NativeSyncSamples.journalLines(fs)).toHaveLength(1)
    })

    test('a native writer that stays mid-commit is reported, not half read', async () => {
        const { fylo, fs } = await NativeSyncSamples.core()
        await NativeSyncSamples.nativeWrite(fylo, fs, TTID.generate(), { title: 'half' })
        await NativeSyncSamples.publish(fs, 3, 'writing')
        const code = await NativeSyncSamples.errorCode(() =>
            fylo.changes(NativeSyncSamples.collection, null)
        )
        expect(code).toBe('ENATIVE_CONCURRENT_WRITE')
        expect(await NativeSyncSamples.journalLines(fs)).toEqual([])
    })

    test('a stable generation with an unapplied log record waits for recovery', async () => {
        const { fylo, fs } = await NativeSyncSamples.core()
        const id = TTID.generate()
        await NativeSyncSamples.nativeWrite(fylo, fs, id, { title: 'logged' })
        await fs.mkdir('/.fylo-transactions/.collections/notes/log', { recursive: true })
        await fs.writeText(NativeSyncSamples.logPath, 'abcdef')
        await NativeSyncSamples.publish(fs, 2, 'stable', 4)
        const code = await NativeSyncSamples.errorCode(() =>
            fylo.changes(NativeSyncSamples.collection, null)
        )
        expect(code).toBe('ENATIVE_CONCURRENT_WRITE')
        await NativeSyncSamples.publish(fs, 2, 'stable', 6)
        const page = await fylo.changes(NativeSyncSamples.collection, null)
        expect(page.changes).toEqual([{ id, kind: 'put' }])
    })

    test('a collection the native engine dropped is not created again', async () => {
        const { fylo, fs } = await NativeSyncSamples.core()
        await NativeSyncSamples.publish(fs, 2)
        await fs.rmdir('/.collections/notes', { recursive: true })
        const page = await fylo.changes(NativeSyncSamples.collection, null)
        expect(page.changes).toEqual([])
        expect(await fs.exists('/.collections/notes')).toBe(false)
    })
})
