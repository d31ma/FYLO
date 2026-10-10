import { describe, expect, test } from 'bun:test'
import { createMemoryFilesystem } from '../../src/browser/core/memory-filesystem.js'
import { BrowserCore } from '../../src/browser/core/engine.js'

// FYLO's loose equality, which the native engine defines: a missing field
// equals only null, arrays and objects never equal a scalar, and mixed scalars
// compare as finite numbers.
const scalarNumber = (value) => {
    if (typeof value !== 'string') return Number(value)
    const numeric = Number(value.trim())
    return Number.isFinite(numeric) ? numeric : null
}
const fyloEqual = (actual, expected) => {
    if (actual === undefined || actual === null) return expected === null
    if (typeof actual === 'object' || expected === null || typeof expected === 'object')
        return false
    if (typeof actual === typeof expected) return actual === expected
    const left = scalarNumber(actual)
    return left !== null && left === scalarNumber(expected)
}

describe('BrowserPrefixIndex', () => {
    test('equality and contains preserve loose numeric, null, and container matches', async () => {
        const fs = createMemoryFilesystem()
        const fylo = new BrowserCore({ fs, root: '/' })
        await fylo.users.create()
        const documents = new Map()
        for (const document of [
            { score: 1 },
            { score: '1' },
            { score: '01' },
            { score: true },
            { score: null },
            {},
            { score: 0 },
            { score: -0 },
            { score: '' },
            { score: ' ' },
            { score: false },
            { score: [] },
            { score: [null] },
            { score: ['1'] },
            { score: ['01'] },
            { score: ['a', 'b'] },
            { score: {} },
            { score: 'Ada' }
        ]) {
            documents.set(await fylo.users.put(document), document)
        }
        for (const value of [
            1,
            '1',
            '01',
            true,
            null,
            0,
            -0,
            '',
            ' ',
            false,
            'a,b',
            '[object Object]',
            'Ada'
        ]) {
            const expected = [...documents]
                .filter(([, document]) => fyloEqual(document.score, value))
                .map(([id]) => id)
                .sort()
            const actual = await collect(fylo, { $ops: [{ score: { $eq: value } }] })
            expect(Object.keys(actual).sort()).toEqual(expected)
            const containsExpected = [...documents]
                .filter(
                    ([, document]) =>
                        Array.isArray(document.score) &&
                        document.score.some((item) => fyloEqual(item, value))
                )
                .map(([id]) => id)
                .sort()
            const containsActual = await collect(fylo, { $ops: [{ score: { $contains: value } }] })
            expect(Object.keys(containsActual).sort()).toEqual(containsExpected)
        }
        await fylo.close()
    })

    test('combines loose contains fallback with an indexed conjunct', async () => {
        const fylo = new BrowserCore({ fs: createMemoryFilesystem(), root: '/' })
        await fylo.users.create()
        const included = await fylo.users.put({ values: ['01'], name: 'Included' })
        await fylo.users.put({ values: ['01'], name: 'Excluded' })
        await fylo.users.put({ values: [2], name: 'Included' })
        expect(
            Object.keys(
                await collect(fylo, {
                    $ops: [{ values: { $contains: 1, $ne: 0 }, name: { $like: 'Incl%' } }]
                })
            )
        ).toEqual([included])
        await fylo.close()
    })

    test('stores server-compatible manifest, snapshot, and WAL files', async () => {
        const fs = createMemoryFilesystem()
        const fylo = new BrowserCore({ fs, root: '/' })
        await fylo['users'].create()
        const id = await fylo['users'].put({
            name: 'Alice',
            score: 42,
            tags: ['browser', 'fylo']
        })

        expect(await fs.exists('/.collections/users/index/manifest.json')).toBe(true)
        expect(await fs.exists('/.collections/users/index/keys.snapshot')).toBe(true)
        const wal = await fs.readText('/.collections/users/index/keys.wal')
        expect(wal).toContain(`+\tname/eq/Alice/${id}`)
        expect(wal).toContain(`+\tname/f/Alice/${id}`)
        expect(wal).toContain(`+\ttags/eq/browser/${id}`)
    })

    test('uses prefix index candidates for exact, like, contains, and range queries', async () => {
        const fs = createMemoryFilesystem()
        const fylo = new BrowserCore({ fs, root: '/' })
        await fylo['users'].create()
        const ada = await fylo['users'].put({ name: 'Ada', score: 10, tags: ['ops'] })
        const grace = await fylo['users'].put({ name: 'Grace', score: 20, tags: ['runtime'] })

        const exact = await collect(fylo, { $ops: [{ name: { $eq: 'Ada' } }] })
        expect(Object.keys(exact)).toEqual([ada])

        const like = await collect(fylo, { $ops: [{ name: { $like: '%ace' } }] })
        expect(Object.keys(like)).toEqual([grace])

        const contains = await collect(fylo, { $ops: [{ tags: { $contains: 'ops' } }] })
        expect(Object.keys(contains)).toEqual([ada])

        const range = await collect(fylo, { $ops: [{ score: { $gte: 15 } }] })
        expect(Object.keys(range)).toEqual([grace])
    })

    test('indexes nested fields, scalar arrays, LIKE helpers, booleans, and numeric ranges', async () => {
        const fs = createMemoryFilesystem()
        const fylo = new BrowserCore({ fs, root: '/' })
        await fylo['users'].create()
        const id = await fylo['users'].put({
            name: 'Alice',
            address: { city: 'Lagos' },
            tags: ['ops', 'browser'],
            score: 42,
            active: true
        })
        const wal = await fs.readText('/.collections/users/index/keys.wal')

        expect(wal).toContain(`+\tname/eq/Alice/${id}`)
        expect(wal).toContain(`+\tname/r/ecilA/${id}`)
        expect(wal).toContain(`+\tname/g3/lic/${id}`)
        expect(wal).toContain(`+\taddress/city/eq/Lagos/${id}`)
        expect(wal).toContain(`+\ttags/eq/ops/${id}`)
        expect(wal).toContain(`+\ttags/eq/browser/${id}`)
        expect(wal).toContain(`+\tscore/eq/42/${id}`)
        expect(wal).toContain(`+\tactive/eq/true/${id}`)
        expect(Object.keys(await collect(fylo, { $ops: [{ active: { $eq: true } }] }))).toEqual([
            id
        ])
        expect(
            Object.keys(await collect(fylo, { $ops: [{ 'address.city': { $eq: 'Lagos' } }] }))
        ).toEqual([id])
        expect(
            Object.keys(await collect(fylo, { $ops: [{ tags: { $contains: 'browser' } }] }))
        ).toEqual([id])
        expect(Object.keys(await collect(fylo, { $ops: [{ name: { $like: 'Ali%' } }] }))).toEqual([
            id
        ])
        expect(Object.keys(await collect(fylo, { $ops: [{ name: { $like: '%ice' } }] }))).toEqual([
            id
        ])
        expect(Object.keys(await collect(fylo, { $ops: [{ name: { $like: '%lic%' } }] }))).toEqual([
            id
        ])
        expect(Object.keys(await collect(fylo, { $ops: [{ score: { $gte: 40 } }] }))).toEqual([id])
    })

    test('a range never matches a value the range index cannot hold', async () => {
        const fylo = new BrowserCore({ fs: createMemoryFilesystem(), root: '/' })
        await fylo.users.create()
        const ten = await fylo.users.put({ n: 10 })
        await fylo.users.put({ n: 'Infinity' })
        const hex = await fylo.users.put({ n: '0x10' })
        const found = await collect(fylo, { $ops: [{ n: { $gt: 5 } }] })
        expect(Object.keys(found).sort()).toEqual([ten, hex].sort())
        await fylo.close()
    })
})

async function collect(fylo, query) {
    const docs = {}
    for await (const doc of fylo['users'].find(query).collect()) Object.assign(docs, doc)
    return docs
}
