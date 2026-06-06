import { describe, expect, test } from 'bun:test'
import { createMemoryFilesystem } from '../../src/browser/core/memory-filesystem.js'
import { BrowserCore } from '../../src/browser/core/engine.js'

describe('BrowserPrefixIndex', () => {
    test('stores server-compatible manifest, snapshot, and WAL files', async () => {
        const fs = createMemoryFilesystem()
        const fylo = new BrowserCore({ fs, root: '/' })
        const id = await fylo.putData('users', {
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
        const ada = await fylo.putData('users', { name: 'Ada', score: 10, tags: ['ops'] })
        const grace = await fylo.putData('users', { name: 'Grace', score: 20, tags: ['runtime'] })

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
        const id = await fylo.putData('users', {
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
})

async function collect(fylo, query) {
    const docs = {}
    for await (const doc of fylo.findDocs('users', query).collect()) Object.assign(docs, doc)
    return docs
}
