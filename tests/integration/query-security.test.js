import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-native-query-security-')
const fylo = new Fylo(root, { versioning: { autoCommit: false } })

afterAll(async () => {
    await rm(root, { recursive: true, force: true })
})

describe('native query security', () => {
    test('LIKE preserves literal regex characters and native underscore semantics', async () => {
        await fylo.values.create()
        const literal = await fylo.values.put({ name: 'a.b[1]_x' })
        await fylo.values.put({ name: 'axb11Zx' })

        const matches = []
        for await (const page of fylo.values
            .find({ $ops: [{ name: { $like: '%a.b[1]_x%' } }] })
            .collect()) {
            matches.push(...Object.keys(page))
        }
        expect(matches).toEqual([literal])

        expect(fylo.engine.queryEngine.matchesOperand('A_a', { $like: 'A_a' })).toBe(true)
        expect(fylo.engine.queryEngine.matchesOperand('Ada', { $like: 'A_a' })).toBe(false)
    })

    test('adversarial LIKE patterns complete in linear time', () => {
        const value = `${'a'.repeat(200_000)}z`
        const matching = `%${'a%'.repeat(20_000)}z`
        const missing = `%${'a%'.repeat(20_000)}x`
        const startedAt = performance.now()

        expect(fylo.engine.queryEngine.matchesOperand(value, { $like: matching })).toBe(true)
        expect(fylo.engine.queryEngine.matchesOperand(value, { $like: missing })).toBe(false)
        expect(performance.now() - startedAt).toBeLessThan(1_000)
    })
})
