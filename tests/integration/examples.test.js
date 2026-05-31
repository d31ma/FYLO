import { describe, expect, test } from 'bun:test'
import { mkdtemp, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import Fylo from '../../src/index.js'
import { validateAgainstHead } from '../../src/schema/validation.js'

const EXAMPLE_ROOT = path.join(process.cwd(), 'examples', 'db')
const EXAMPLE_SCHEMA_DIR = path.join(EXAMPLE_ROOT, 'schemas')

describe('example production root', () => {
    test('seeded mock data is readable through FYLO', async () => {
        const fylo = new Fylo(EXAMPLE_ROOT)

        const user = await fylo.getDoc('users', '4V6329YC0F2').once()
        expect(user['4V6329YC0F2']).toEqual({
            name: 'Ada Lovelace',
            role: 'admin',
            team: 'platform',
            tags: ['math', 'storage']
        })

        let admins = {}
        for await (const data of fylo
            .findDocs('users', {
                $ops: [{ role: { $eq: 'admin' } }]
            })
            .collect()) {
            admins = { ...admins, ...data }
        }
        expect(Object.keys(admins)).toEqual(['4V6329YC0F2'])

        let openOrders = {}
        for await (const data of fylo
            .findDocs('orders', {
                $ops: [{ status: { $eq: 'open' } }]
            })
            .collect()) {
            openOrders = { ...openOrders, ...data }
        }
        expect(openOrders['4V6329YC0R0'].orderNo).toBe('ORD-1001')
    })

    test('example collections include matching versioned schemas', async () => {
        const user = await validateAgainstHead(
            'users',
            {
                name: 'Ada Lovelace',
                role: 'admin',
                team: 'platform',
                tags: ['math', 'storage']
            },
            { schemaDir: EXAMPLE_SCHEMA_DIR }
        )
        expect(user._v).toBe('v1')

        const order = await validateAgainstHead(
            'orders',
            {
                orderNo: 'ORD-1001',
                userId: '4V6329YC0F2',
                status: 'open',
                total: 42.5
            },
            { schemaDir: EXAMPLE_SCHEMA_DIR }
        )
        expect(order._v).toBe('v1')
    })

    test('startup creates missing collections declared by schema manifests', async () => {
        const previousSchema = process.env.FYLO_SCHEMA
        const root = await mkdtemp(path.join(os.tmpdir(), 'fylo-schema-startup-'))
        process.env.FYLO_SCHEMA = EXAMPLE_SCHEMA_DIR

        try {
            const fylo = new Fylo(root)
            await fylo.ready()

            expect((await fylo.inspectCollection('article')).exists).toBe(true)
            expect((await fylo.inspectCollection('orders')).exists).toBe(true)
            expect((await fylo.inspectCollection('users')).exists).toBe(true)
            expect((await fylo.inspectCollection('report')).exists).toBe(false)
        } finally {
            if (previousSchema === undefined) delete process.env.FYLO_SCHEMA
            else process.env.FYLO_SCHEMA = previousSchema
            await rm(root, { recursive: true, force: true })
        }
    })
})
