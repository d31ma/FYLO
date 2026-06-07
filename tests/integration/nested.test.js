import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { rm } from 'node:fs/promises'
import Fylo from '../../src/index.js'
import { usersURL } from '../data.js'
import { createTestRoot } from '../helpers/root.js'
const USERS = 'nst-user'
let insertedCount = 0
let sampleId
const root = await createTestRoot('fylo-nested-')
const fylo = new Fylo(root)
beforeAll(async () => {
    await fylo[USERS].create()
    try {
        insertedCount = await fylo[USERS].import(new URL(usersURL))
    } catch {}
    for await (const data of fylo[USERS].find({ $limit: 1, $onlyIds: true }).collect()) {
        sampleId = data
    }
})
afterAll(async () => {
    await fylo[USERS].drop()
    await rm(root, { recursive: true, force: true })
})
describe('NO-SQL', async () => {
    test('SELECT ALL — nested documents are returned', async () => {
        let results = {}
        for await (const data of fylo[USERS].find().collect()) {
            results = { ...results, ...data }
        }
        expect(Object.keys(results).length).toBe(insertedCount)
    })
    test('GET ONE — top-level fields are reconstructed correctly', async () => {
        const result = await fylo[USERS].get(sampleId).once()
        const user = result[sampleId]
        expect(user).toBeDefined()
        expect(typeof user.name).toBe('string')
        expect(typeof user.email).toBe('string')
        expect(typeof user.phone).toBe('string')
    })
    test('GET ONE — first-level nested object is reconstructed correctly', async () => {
        const result = await fylo[USERS].get(sampleId).once()
        const user = result[sampleId]
        expect(user.address).toBeDefined()
        expect(typeof user.address.city).toBe('string')
        expect(typeof user.address.street).toBe('string')
        expect(typeof user.address.zipcode).toBe('string')
    })
    test('GET ONE — deeply nested object is reconstructed correctly', async () => {
        const result = await fylo[USERS].get(sampleId).once()
        const user = result[sampleId]
        expect(user.address.geo).toBeDefined()
        expect(typeof user.address.geo.lat).toBe('number')
        expect(typeof user.address.geo.lng).toBe('number')
    })
    test('GET ONE — second nested object is reconstructed correctly', async () => {
        const result = await fylo[USERS].get(sampleId).once()
        const user = result[sampleId]
        expect(user.company).toBeDefined()
        expect(typeof user.company.name).toBe('string')
        expect(typeof user.company.catchPhrase).toBe('string')
        expect(typeof user.company.bs).toBe('string')
    })
    test('SELECT — nested values are not corrupted across documents', async () => {
        for await (const data of fylo[USERS].find().collect()) {
            const [, user] = Object.entries(data)[0]
            expect(user.address).toBeDefined()
            expect(user.address.geo).toBeDefined()
            expect(typeof user.address.geo.lat).toBe('number')
            expect(user.company).toBeDefined()
        }
    })
    test('$select — returns only requested top-level fields', async () => {
        let results = {}
        for await (const data of fylo[USERS].find({ $select: ['name', 'email'] }).collect()) {
            results = { ...results, ...data }
        }
        const users = Object.values(results)
        const onlyNameAndEmail = users.every((u) => u.name && u.email && !u.phone && !u.address)
        expect(onlyNameAndEmail).toBe(true)
    })
    test('$eq on nested string field — query by city', async () => {
        const result = await fylo[USERS].get(sampleId).once()
        const targetCity = result[sampleId].address.city
        let results = {}
        for await (const data of fylo[USERS].find({
            $ops: [{ ['address/city']: { $eq: targetCity } }]
        }).collect()) {
            results = { ...results, ...data }
        }
        const matchingUsers = Object.values(results)
        const allMatch = matchingUsers.every((u) => u.address.city === targetCity)
        expect(allMatch).toBe(true)
        expect(matchingUsers.length).toBeGreaterThan(0)
    })
})
describe('SQL — dot notation', async () => {
    test('WHERE with dot notation — first-level nested field', async () => {
        const result = await fylo[USERS].get(sampleId).once()
        const targetCity = result[sampleId].address.city
        const results = await fylo._sql(
            `SELECT * FROM ${USERS} WHERE address.city = '${targetCity}'`
        )
        const users = Object.values(results)
        const allMatch = users.every((u) => u.address.city === targetCity)
        expect(allMatch).toBe(true)
        expect(users.length).toBeGreaterThan(0)
    })
    test('WHERE with dot notation — deeply nested field', async () => {
        const result = await fylo[USERS].get(sampleId).once()
        const targetLat = result[sampleId].address.geo.lat
        const results = await fylo._sql(
            `SELECT * FROM ${USERS} WHERE address.geo.lat = '${targetLat}'`
        )
        const users = Object.values(results)
        const allMatch = users.every((u) => u.address.geo.lat === targetLat)
        expect(allMatch).toBe(true)
        expect(users.length).toBeGreaterThan(0)
    })
    test('WHERE with dot notation — second nested object', async () => {
        const result = await fylo[USERS].get(sampleId).once()
        const targetCompany = result[sampleId].company.name
        const results = await fylo._sql(
            `SELECT * FROM ${USERS} WHERE company.name = '${targetCompany}'`
        )
        const users = Object.values(results)
        const allMatch = users.every((u) => u.company.name === targetCompany)
        expect(allMatch).toBe(true)
        expect(users.length).toBeGreaterThan(0)
    })
    test('SELECT with dot notation in WHERE — partial field selection', async () => {
        const result = await fylo[USERS].get(sampleId).once()
        const targetCity = result[sampleId].address.city
        const results = await fylo._sql(
            `SELECT name, email FROM ${USERS} WHERE address.city = '${targetCity}'`
        )
        const users = Object.values(results)
        expect(users.length).toBeGreaterThan(0)
        expect(users.every((u) => u.name && u.email && !u.phone)).toBe(true)
    })
})
