import { describe, expect, test } from 'bun:test'
import { createBrowserClient } from '../../src/browser/client.js'
import { BrowserQueryEngine } from '../../src/browser/core/query.js'
import { matchesLike } from '../../src/query/like.js'
import { Parser } from '../../src/query/parser.js'

describe('browser query security', () => {
    test('parses the reserved-key JOIN reproduction without prototype mutation', () => {
        const query = Parser.parse(
            'SELECT * FROM leftdocs INNER JOIN rightdocs ON __proto__ = constructor'
        )

        expect(Object.getPrototypeOf(query.$on)).toBeNull()
        expect(Object.hasOwn(query.$on, '__proto__')).toBe(true)
        expect(query.$on.__proto__).toEqual({ $eq: 'constructor' })
        expect({}.polluted).toBeUndefined()
    })

    test('joins and groups __proto__, constructor, and prototype as data', async () => {
        const db = createBrowserClient({ worker: false })
        await db.collection('leftdocs').create()
        await db.collection('rightdocs').create()
        await db
            .collection('leftdocs')
            .put(
                JSON.parse(
                    '{"__proto__":"shared","constructor":"left","prototype":"left","group":"__proto__"}'
                )
            )
        await db
            .collection('rightdocs')
            .put(JSON.parse('{"__proto__":"right","constructor":"shared","prototype":"right"}'))

        const on = JSON.parse('{"__proto__":{"$eq":"constructor"}}')
        const joined = await db.browser.join({
            $leftCollection: 'leftdocs',
            $rightCollection: 'rightdocs',
            $mode: 'inner',
            $on: on,
            $groupby: 'group'
        })

        expect(Object.getPrototypeOf(joined)).toBeNull()
        expect(Object.hasOwn(joined, '__proto__')).toBe(true)
        expect(Object.keys(joined.__proto__)).toHaveLength(1)
        expect({}.polluted).toBeUndefined()
        await db.close()
    })

    test('round-trips reserved metadata keys without changing prototypes', async () => {
        const db = createBrowserClient({ worker: false })
        await db.users.create()
        const id = await db.users.put({ name: 'Ada' })
        await db.users.put(id).metadata({
            constructor: 'ctor',
            prototype: 'prototype',
            nested: JSON.parse('{"__proto__":"nested"}')
        })

        const metadata = await db.users.get(id).metadata()
        expect(Object.getPrototypeOf(metadata)).toBeNull()
        expect(metadata.constructor).toBe('ctor')
        expect(metadata.prototype).toBe('prototype')
        expect(Object.hasOwn(metadata.nested, '__proto__')).toBe(true)
        expect(metadata.nested.__proto__).toBe('nested')
        expect({}.polluted).toBeUndefined()
        await db.close()
    })

    test('LIKE supports percent and underscore without regular expressions', async () => {
        expect(matchesLike('Ada Lovelace', 'Ada%')).toBe(true)
        expect(matchesLike('Ada', 'A_a')).toBe(true)
        expect(matchesLike('A.a', 'A.a')).toBe(true)
        expect(matchesLike('Ada', 'A__')).toBe(true)
        expect(matchesLike('Ada', 'A_')).toBe(false)

        const engine = new BrowserQueryEngine({
            index: {
                async candidateDocIds() {
                    return null
                }
            }
        })
        expect(engine.matchesLike('a'.repeat(200_000) + 'z', `%${'a%'.repeat(20_000)}z`)).toBe(true)
        expect(engine.matchesLike('a'.repeat(200_000) + 'z', `%${'a%'.repeat(20_000)}x`)).toBe(
            false
        )

        const db = createBrowserClient({ worker: false })
        await db.users.create()
        const ada = await db.users.put({ name: 'Ada' })
        await db.users.put({ name: 'Adaa' })
        const matches = []
        for await (const page of db.users.find({ $ops: [{ name: { $like: 'A_a' } }] }).collect()) {
            matches.push(...Object.keys(page))
        }
        expect(matches).toEqual([ada])
        await db.close()
    })
})
