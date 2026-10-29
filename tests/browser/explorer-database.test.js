import { describe, expect, test } from 'bun:test'

// Tachyon injects lifecycle decorators while compiling components; the pure
// helpers under test need only a harmless binding.
globalThis.onMount = () => {}
const {
    default: WebsiteExplorer,
    collectionHasSchema,
    databaseEngineUrl,
    databaseReportRows,
    databaseWorkerUrl,
    schemaFailure
} = await import('../../explorer/client/components/explorer/app/tac.js')
delete globalThis.onMount

describe('Explorer .vdbx view', () => {
    test('loads the database Worker and engine from its own asset folder', () => {
        expect(databaseWorkerUrl('abc123', 'https://fx.example')).toBe(
            'https://fx.example/shared/assets/vdbx-worker.js?v=abc123'
        )
        expect(databaseEngineUrl('', 'https://fx.example')).toBe(
            'https://fx.example/shared/assets/fylo-browser.wasm'
        )
        expect(() => databaseEngineUrl('../x', 'https://fx.example')).toThrow()
    })

    test('turns the container report into collection and extent rows', () => {
        const rows = databaseReportRows({
            format: 'fylo.vdbx',
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

describe('Explorer schemas (ADR 0017)', () => {
    test('a collection with a registry or a checked mode has a schema', () => {
        expect(collectionHasSchema({ schemas: 'registry', schemaHead: 2 })).toBe(true)
        expect(collectionHasSchema({ validation: 'strict' })).toBe(true)
        expect(collectionHasSchema({ validation: 'warn' })).toBe(true)
        expect(collectionHasSchema({ validation: 'off' })).toBe(false)
        expect(collectionHasSchema({}, true)).toBe(true)
        expect(collectionHasSchema({})).toBe(false)
        expect(collectionHasSchema(null)).toBe(false)
    })

    test('reads the failing field out of an ESCHEMA refusal', () => {
        const failure = schemaFailure(
            Object.assign(new Error("RegEx pattern fails for property 'email' in schema 'v2'"), {
                code: 'ESCHEMA'
            })
        )
        expect(failure).toEqual({
            code: 'ESCHEMA',
            path: 'email',
            field: 'email',
            message: "RegEx pattern fails for property 'email' in schema 'v2'"
        })
        expect(
            schemaFailure({
                code: 'ESCHEMA',
                message: "Type mismatch for 'tags[2]' in schema 'v1'"
            })?.field
        ).toBe('tags')
        expect(
            schemaFailure(
                new Error(
                    "ESCHEMA: Property 'address.city' cannot be null or undefined in schema 'v3'"
                )
            )
        ).toMatchObject({ path: 'address.city', field: 'address' })
    })

    test('ESCHEMAUNAVAILABLE names no field, and other errors are not schema failures', () => {
        expect(schemaFailure({ code: 'ESCHEMAUNAVAILABLE', message: 'pattern needs \\b' })).toEqual(
            { code: 'ESCHEMAUNAVAILABLE', path: '', field: '', message: 'pattern needs \\b' }
        )
        expect(schemaFailure(new Error('Draft is not valid JSON'))).toBeNull()
        expect(schemaFailure({ code: 'EIO', message: "Property 'x'" })).toBeNull()
    })

    test('writes stay off for a schema collection even with writes enabled', () => {
        const explorer = new WebsiteExplorer()
        explorer.writable = true
        explorer.schemaLocked = { people: true }
        explorer.active = 'people'
        expect(explorer.writable).toBe(false)
        expect(explorer.rootWritable).toBe(true)
        explorer.active = 'notes'
        expect(explorer.writable).toBe(true)
    })

    test('the field editor marks the field a schema refusal names', () => {
        const explorer = new WebsiteExplorer()
        explorer.editFields = [
            { key: 'name', type: 'string', value: 'x', regex: '' },
            { key: 'email', type: 'string', value: 'y', regex: '' }
        ]
        explorer.fieldError = schemaFailure({
            code: 'ESCHEMA',
            message: "RegEx pattern fails for property 'email' in schema 'v2'"
        })
        expect(explorer.editView().map((field) => field.invalid)).toEqual([false, true])
        expect(explorer.fieldErrorOffEditor()).toBe(false)
        explorer.fieldValueInput('email', { target: { value: 'a@b.c' } })
        expect(explorer.fieldError).toBeNull()

        explorer.active = 'people'
        explorer.fieldError = schemaFailure({ code: 'ESCHEMAUNAVAILABLE', message: '' })
        expect(explorer.fieldErrorOffEditor()).toBe(true)
        expect(explorer.schemaErrorText()).toContain("cannot validate people's schema")
    })
})
