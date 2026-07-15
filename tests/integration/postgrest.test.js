import { describe, expect, test } from 'bun:test'
import { queryFromSearch } from '../../src/query/postgrest.js'

describe('PostgREST filter grammar', () => {
    test('translates filters (AND semantics), limit, select, and onlyIds', () => {
        expect(queryFromSearch('role=eq.admin&age=gte.30&limit=5&select=name,role')).toEqual({
            $limit: 5,
            $select: ['name', 'role'],
            $ops: [{ role: { $eq: 'admin' }, age: { $gte: 30 } }]
        })
        expect(queryFromSearch('onlyIds=true&name=like.Ada%25')).toEqual({
            $onlyIds: true,
            $ops: [{ name: { $like: 'Ada%' } }]
        })
        // repeating a field merges its operators into one range
        expect(queryFromSearch('age=gte.18&age=lt.30')).toEqual({
            $ops: [{ age: { $gte: 18, $lt: 30 } }]
        })
        // bare value and unknown operator both fall back to equality; scalars coerce
        expect(queryFromSearch('active=true&score=3.5&tag=weird.x')).toEqual({
            $ops: [{ active: { $eq: true }, score: { $eq: 3.5 }, tag: { $eq: 'weird.x' } }]
        })
        expect(() => queryFromSearch('limit=-1')).toThrow('Invalid limit')
    })

    test('treats prototype-reserved filter names as ordinary own keys', () => {
        const query = queryFromSearch(
            '__proto__=eq.proto&constructor=eq.ctor&prototype=eq.prototype'
        )
        const filters = query.$ops[0]

        expect(Object.getPrototypeOf(filters)).toBeNull()
        expect(Object.hasOwn(filters, '__proto__')).toBe(true)
        expect(filters.__proto__).toEqual({ $eq: 'proto' })
        expect(filters.constructor).toEqual({ $eq: 'ctor' })
        expect(filters.prototype).toEqual({ $eq: 'prototype' })
        expect({}.polluted).toBeUndefined()
    })
})
