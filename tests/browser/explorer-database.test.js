import { describe, expect, test } from 'bun:test'

// Tachyon injects lifecycle decorators while compiling components; the pure
// helpers under test need only a harmless binding.
globalThis.onMount = () => {}
const { databaseEngineUrl, databaseReportRows, databaseWorkerUrl } =
    await import('../../explorer/client/components/explorer/app/tac.js')
delete globalThis.onMount

describe('Explorer .fxdb view', () => {
    test('loads the database Worker and engine from its own asset folder', () => {
        expect(databaseWorkerUrl('abc123', 'https://fx.example')).toBe(
            'https://fx.example/shared/assets/fxdb-worker.js?v=abc123'
        )
        expect(databaseEngineUrl('', 'https://fx.example')).toBe(
            'https://fx.example/shared/assets/fylo-browser.wasm'
        )
        expect(() => databaseEngineUrl('../x', 'https://fx.example')).toThrow()
    })

    test('turns the container report into collection and extent rows', () => {
        const rows = databaseReportRows({
            format: 'fylo.fxdb',
            extents: {
                free: 3,
                released: 1,
                byKind: { segment: { extents: 4, sealed: 2, liveBytes: 1024 } }
            },
            collections: [
                { name: 'orders', id: 1, dropped: false, records: 12, checkpointLsn: 9 },
                { name: 'old', id: 2, dropped: true, records: 0, checkpointLsn: null }
            ]
        })
        expect(rows.collections).toEqual([
            { name: 'orders', id: 1, records: 12, checkpoint: 9, dropped: false },
            { name: 'old', id: 2, records: 0, checkpoint: null, dropped: true }
        ])
        expect(rows.extents).toEqual([
            { kind: 'segment', extents: 4, sealed: 2, liveBytes: 1024 },
            { kind: 'free', extents: 3, sealed: 0, liveBytes: 0 },
            { kind: 'released', extents: 1, sealed: 0, liveBytes: 0 }
        ])
    })

    test('an empty database has no collections and only free extents', () => {
        const rows = databaseReportRows({ extents: { free: 0, released: 0, byKind: {} } })
        expect(rows.collections).toEqual([])
        expect(rows.extents.map((row) => row.kind)).toEqual(['free', 'released'])
    })
})
