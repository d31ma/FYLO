import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo, { FyloAuthError } from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const root = await createTestRoot('fylo-auth-')

afterAll(async () => {
    await rm(root, { recursive: true, force: true })
})

describe('auth policy wrapper', () => {
    test('as() fails closed when RLS is not enabled', () => {
        const fylo = new Fylo(root)
        expect(() => fylo.as({ subjectId: 'user-1' })).toThrow('FYLO RLS is not enabled')
    })

    test('authorized scoped client delegates public document operations', async () => {
        const calls = []
        const fylo = new Fylo(root, { rls: true })
        const scoped = fylo.as({ subjectId: 'user-1', tenantId: 'tenant-a', roles: ['writer'] })
        const collection = 'auth-allowed'

        await scoped[collection].create()
        const id = await scoped[collection].put({
            tenantId: 'tenant-a',
            title: 'Allowed'
        })

        const doc = await scoped[collection].get(id).once()
        expect(doc[id].title).toBe('Allowed')

        const results = {}
        for await (const value of scoped[collection]
            .find({
                $ops: [{ tenantId: { $eq: 'tenant-a' } }]
            })
            .collect()) {
            Object.assign(results, value)
        }
        expect(results[id].title).toBe('Allowed')

        const nextId = await scoped[collection].patch(id, { title: 'Updated' })
        expect(nextId).toBe(id)

        let exported = 0
        for await (const _doc of scoped[collection].export()) exported++
        expect(exported).toBe(1)

        await expect(scoped[collection].delete(nextId)).rejects.toBeInstanceOf(FyloAuthError)

        await fylo[collection].drop()
    })

    test('denied reads do not touch storage', async () => {
        const fylo = new Fylo(root, { rls: true })
        const collection = 'auth-denied'
        await fylo[collection].create()
        const id = await fylo[collection].put({ title: 'Private' })
        const scoped = fylo.as({ subjectId: 'blocked-user' })

        await expect(scoped[collection].get(id).once()).rejects.toBeInstanceOf(FyloAuthError)

        await fylo[collection].drop()
    })
})
