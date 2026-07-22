import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-explain-')

describe('prepared SQL plans and EXPLAIN', () => {
    afterAll(async () => {
        await rm(root, { recursive: true, force: true })
    })

    test('EXPLAIN reports the selected prefix-index access path without executing', async () => {
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo.people.create()
        await fylo.people.put({ name: 'Ada', active: true })

        const plan = await fylo._sql("EXPLAIN SELECT * FROM people WHERE name = 'Ada'")

        expect(plan.operation).toBe('SELECT')
        expect(plan.collection).toBe('people')
        expect(plan.access.some((step) => step.kind === 'prefix-index')).toBe(true)
        expect(plan.executed).toBe(false)
        const rows = await fylo._sql("SELECT * FROM people WHERE name = 'Ada'")
        expect(Object.keys(rows)).toHaveLength(1)
    })

    test('a prepared statement reuses its parsed plan', async () => {
        const fylo = new Fylo(root, { versioning: { autoCommit: false } })
        await fylo.prepareddocs.create()
        const id = await fylo.prepareddocs.put({ name: 'Grace' })
        const statement = fylo.prepare("SELECT * FROM prepareddocs WHERE name = 'Grace'")

        expect(statement.explain().operation).toBe('SELECT')
        expect(Object.isFrozen(statement.plan)).toBe(true)
        expect(Object.isFrozen(statement.plan.ast)).toBe(true)
        expect(await statement.execute()).toEqual({ [id]: { name: 'Grace' } })
        expect(await statement.execute()).toEqual({ [id]: { name: 'Grace' } })
    })
})
