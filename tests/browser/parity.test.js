import { describe, expect, test } from 'bun:test'
import { BrowserPrefixIndexCodec } from '../../src/browser/core/prefix-index.js'

const id = '4UUB1111111'

describe('FYLO browser parity guardrails', () => {
    test('prefix-index entries are deterministic across supported document shapes', async () => {
        const docs = [
            {
                name: 'Alice',
                active: true,
                score: 42,
                empty: null
            },
            {
                name: 'Grace',
                address: { city: 'Lagos', zip: 100001 },
                tags: ['ops', 'browser', null]
            },
            {
                title: 'long value falls back safely',
                body: 'x'.repeat(256),
                decimal: 12.5
            },
            {
                name: 'Contains trigram',
                note: 'browser-runtime'
            }
        ]

        for (const doc of docs) {
            const first = await BrowserPrefixIndexCodec.entriesForDocument('users', id, doc)
            const second = await BrowserPrefixIndexCodec.entriesForDocument('users', id, doc)
            expect(new Set(second)).toEqual(new Set(first))
        }
    })

    test('keeps unsupported arrays of objects fail-closed', async () => {
        await expect(
            BrowserPrefixIndexCodec.entriesForDocument('users', id, { tags: [{ name: 'ops' }] })
        ).rejects.toThrow('Cannot index an array of objects')
    })

    test('records a local benchmark for browser index generation', async () => {
        const doc = {
            name: 'Alice',
            address: { city: 'Lagos', country: 'NG' },
            tags: ['ops', 'browser', 'browser'],
            score: 42,
            note: 'browser-runtime'
        }
        const iterations = 100

        const startedAt = performance.now()
        for (let i = 0; i < iterations; i++) {
            await BrowserPrefixIndexCodec.entriesForDocument('users', id, doc)
        }
        const elapsedMs = performance.now() - startedAt

        expect(Number.isFinite(elapsedMs)).toBe(true)
        expect(await BrowserPrefixIndexCodec.entriesForDocument('users', id, doc)).toContain(
            `name/eq/Alice/${id}`
        )
    })
})
