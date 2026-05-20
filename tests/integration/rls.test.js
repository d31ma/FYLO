import { afterAll, describe, expect, test, beforeAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo, { FyloAuthError } from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'
import { _resetRulesCache } from '../../src/security/rules/loader.js'
import { effectiveReadFilter } from '../../src/security/rules/engine.js'

const root = await createTestRoot('fylo-rls-')
const fylo = new Fylo({ root, rls: true })

beforeAll(async () => {
    await fylo.createCollection('report')
})

afterAll(async () => {
    _resetRulesCache()
    await rm(root, { recursive: true, force: true })
})

describe('RLS read.filter', () => {
    test('user A cannot see user B docs via getLatest', async () => {
        const idA = await fylo.putData('report', {
            tenantId: 'tenant-a',
            title: 'A Report',
            public: false
        })
        const idB = await fylo.putData('report', {
            tenantId: 'tenant-b',
            title: 'B Report',
            public: false
        })

        const userA = fylo.as({ subjectId: 'user-a', tenantId: 'tenant-a' })
        const latestA = await userA.getLatest('report', idA)
        expect(latestA[idA]).toBeDefined()
        expect(latestA[idA].title).toBe('A Report')

        // user-a must NOT see tenant-b's doc — returns empty envelope
        const stolen = await userA.getLatest('report', idB)
        expect(Object.keys(stolen)).toHaveLength(0)
    })

    test('invisible docs dropped from findDocs result stream', async () => {
        const userA = fylo.as({ subjectId: 'user-a', tenantId: 'tenant-a' })
        const all = []
        for await (const doc of userA.findDocs('report').collect()) {
            all.push(doc)
        }
        // All returned docs must belong to tenant-a (the only tenant userA belongs to)
        for (const doc of all) {
            const [, data] = Object.entries(doc)[0]
            expect(data.tenantId).toBe('tenant-a')
        }
    })
})

describe('RLS update.filter', () => {
    let idA, idB
    beforeAll(async () => {
        idA = await fylo.putData('report', {
            tenantId: 'tenant-a',
            title: 'User A Doc'
        })
        idB = await fylo.putData('report', {
            tenantId: 'tenant-b',
            title: 'User B Doc'
        })
    })

    test('user cannot patch a doc outside their tenant', async () => {
        const userA = fylo.as({ subjectId: 'user-a', tenantId: 'tenant-a' })
        // idB belongs to tenant-b — update should be denied by update.filter
        await expect(
            userA.patchDoc('report', { [idB]: { title: 'Hacked' } })
        ).rejects.toBeInstanceOf(FyloAuthError)
    })

    test('user can patch their own doc', async () => {
        const userA = fylo.as({ subjectId: 'user-a', tenantId: 'tenant-a' })
        const newId = await userA.patchDoc('report', { [idA]: { title: 'Updated by A' } })
        expect(newId).not.toBe(idA)
        const latest = await userA.getLatest('report', newId)
        expect(latest[newId].title).toBe('Updated by A')
    })
})

describe('RLS update.fields', () => {
    let adminId
    beforeAll(async () => {
        adminId = await fylo.putData('report', {
            tenantId: 'tenant-a',
            title: 'Admin Doc'
        })
    })

    test('patch rejected when field not in allowed fields list', async () => {
        const admin = fylo.as({ subjectId: 'admin', tenantId: 'tenant-a', roles: ['admin'] })
        // admin role is restricted to only 'title' field via update.fields
        await expect(
            admin.patchDoc('report', {
                [adminId]: { title: 'OK title', internalNote: 'secret' }
            })
        ).rejects.toBeInstanceOf(FyloAuthError)
    })

    test('patch allowed when all fields are in allowed fields list', async () => {
        const admin = fylo.as({ subjectId: 'admin', tenantId: 'tenant-a', roles: ['admin'] })
        const newId = await admin.patchDoc('report', {
            [adminId]: { title: 'Admin updated title' }
        })
        expect(newId).not.toBe(adminId)
        const latest = await admin.getLatest('report', newId)
        expect(latest[newId].title).toBe('Admin updated title')
    })
})

describe('RLS delete.filter', () => {
    let idA, idB
    beforeAll(async () => {
        idA = await fylo.putData('report', {
            tenantId: 'tenant-a',
            title: 'Delete A'
        })
        idB = await fylo.putData('report', {
            tenantId: 'tenant-b',
            title: 'Delete B'
        })
    })

    test('user cannot delete a doc outside their tenant', async () => {
        const userA = fylo.as({ subjectId: 'user-a', tenantId: 'tenant-a' })
        await expect(userA.delDoc('report', idB)).rejects.toBeInstanceOf(FyloAuthError)
    })
})

describe('effectiveReadFilter', () => {
    test('returns null when no rules file exists for collection', async () => {
        // Create a collection with no rules file — effectiveReadFilter should return null
        await fylo.createCollection('norules')
        await fylo.putData('norules', { title: 'orphan' })
        const userA = fylo.as({ subjectId: 'user-a', tenantId: 'tenant-a' })
        const filter = await effectiveReadFilter({
            collection: 'norules',
            schemaDir: process.env.FYLO_SCHEMA,
            auth: { subjectId: 'user-a', tenantId: 'tenant-a' }
        })
        expect(filter).toBeNull()
        await fylo.dropCollection('norules')
    })
})

describe('RLS projection guard', () => {
    test('projection queries are refused on RLS-protected collections', async () => {
        const userA = fylo.as({ subjectId: 'user-a', tenantId: 'tenant-a' })
        // $select would yield flat rows that have no envelope to filter against;
        // RLS cannot be enforced, so the wrapper must refuse.
        await expect(async () => {
            for await (const _ of userA.findDocs('report', { $select: ['title'] }).collect()) {
                /* should never reach here */
            }
        }).toThrow(/projection/)
    })
})
