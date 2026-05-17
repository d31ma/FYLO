import { describe, expect, test } from 'bun:test'
import path from 'node:path'
import Fylo from '../../src/index.js'

const EXAMPLE_ROOT = path.join(process.cwd(), 'examples', 'db')

describe('example production root', () => {
    test('seeded mock data is readable through FYLO', async () => {
        const fylo = new Fylo({ root: EXAMPLE_ROOT })

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
})
