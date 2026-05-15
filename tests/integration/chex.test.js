import { test, expect, describe, beforeAll, afterAll } from 'bun:test'
import { mkdir, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import {
    validateData,
    ValidationError,
    SchemaLoadError,
    ConfigError,
    InvalidNameError
} from '@d31ma/chex'

const TEST_DIR = path.join(import.meta.dir, '..', 'schemas', '_chex-test')
const COLLECTION = 'user'

const USER_SCHEMA = {
    id: '^[0-9]+$',
    name: '^[A-Za-z ]+$',
    'email?': '^[a-z0-9@.]+$',
    'age?': '^[0-9]{1,3}$',
    address: {
        street: '^.+$',
        'city?': '^[A-Za-z ]+$'
    }
}

async function setupSchemas() {
    await mkdir(TEST_DIR, { recursive: true })
    await writeFile(
        path.join(TEST_DIR, `${COLLECTION}.json`),
        JSON.stringify(USER_SCHEMA, null, 2)
    )
}

async function teardownSchemas() {
    await rm(TEST_DIR, { recursive: true, force: true })
}

beforeAll(setupSchemas)
afterAll(teardownSchemas)

describe('CHEX direct integration', () => {
    describe('validateData() — happy paths', () => {
        test('validates a complete document successfully', async () => {
            const result = await validateData(COLLECTION, {
                id: '42',
                name: 'Alice',
                email: 'alice@example.com',
                age: '30',
                address: { street: '123 Main St', city: 'Portland' }
            }, { schemaDir: TEST_DIR })
            expect(result).toBeDefined()
            expect(result.id).toBe('42')
            expect(result.name).toBe('Alice')
        })

        test('returns validated data unchanged on success', async () => {
            const input = {
                id: '1',
                name: 'Bob',
                address: { street: '456 Oak Ave' }
            }
            const output = await validateData(COLLECTION, input, {
                schemaDir: TEST_DIR
            })
            expect(output).toEqual(input)
        })

        test('validates with nullable fields omitted', async () => {
            const result = await validateData(COLLECTION, {
                id: '99',
                name: 'Carol',
                address: { street: '789 Pine Rd' }
            }, { schemaDir: TEST_DIR })
            expect(result.name).toBe('Carol')
        })

        test('validates with nullable fields set to null (rejected by regex but accepted as nullable)', async () => {
            // Null values for nullable fields should be allowed
            const result = await validateData(COLLECTION, {
                id: '3',
                name: 'Dave',
                email: null,
                address: { street: '10 Downing St' }
            }, { schemaDir: TEST_DIR })
            expect(result.name).toBe('Dave')
        })

        test('validates numeric fields as strings', async () => {
            const result = await validateData(COLLECTION, {
                id: 7, // number, coerced to '7' by chex
                name: 'Eve',
                address: { street: 'Binary Ln' }
            }, { schemaDir: TEST_DIR })
            expect(result.id).toBe(7)
        })
    })

    describe('validateData() — validation errors', () => {
        test('rejects document with undeclared property', async () => {
            await expect(
                validateData(COLLECTION, {
                    id: '1',
                    name: 'Frank',
                    undeclared: 'surprise',
                    address: { street: 'Hidden Rd' }
                }, { schemaDir: TEST_DIR })
            ).rejects.toThrow()
        })

        test('rejects document missing required non-nullable field', async () => {
            await expect(
                validateData(COLLECTION, {
                    id: '2',
                    // name missing
                    address: { street: 'Nowhere' }
                }, { schemaDir: TEST_DIR })
            ).rejects.toThrow()
        })

        test('rejects document with value failing regex pattern', async () => {
            await expect(
                validateData(COLLECTION, {
                    id: 'not-a-number',
                    name: 'Grace',
                    address: { street: 'Pattern St' }
                }, { schemaDir: TEST_DIR })
            ).rejects.toThrow()
        })

        test('rejects document with invalid nested field', async () => {
            await expect(
                validateData(COLLECTION, {
                    id: '5',
                    name: 'Hank',
                    address: { street: '' } // empty string fails ^.+$
                }, { schemaDir: TEST_DIR })
            ).rejects.toThrow()
        })
    })

    describe('validateData() — error types', () => {
        test('throws InvalidNameError for invalid collection name', async () => {
            await expect(
                validateData('invalid/collection!', {}, { schemaDir: TEST_DIR })
            ).rejects.toBeInstanceOf(InvalidNameError)
        })

        test('throws ValidationError for data that fails schema', async () => {
            try {
                await validateData(COLLECTION, { id: 'x', name: 'Ivy', address: { street: 'Err Ln' } }, { schemaDir: TEST_DIR })
                expect(false).toBe(true) // should not reach
            } catch (err) {
                expect(err).toBeInstanceOf(ValidationError)
            }
        })

        test('ValidationError is also an instance of Error', async () => {
            try {
                await validateData(COLLECTION, { id: 'y', name: 'Jack', address: { street: 'Err Ln' } }, { schemaDir: TEST_DIR })
            } catch (err) {
                expect(err).toBeInstanceOf(Error)
            }
        })
    })

    describe('validateData() — schema loading', () => {
        test('throws SchemaLoadError when schemaDir points nowhere', async () => {
            await expect(
                validateData('nonexistent', { foo: 'bar' }, {
                    schemaDir: '/tmp/definitely-not-a-real-directory-12345'
                })
            ).rejects.toBeInstanceOf(SchemaLoadError)
        })

        test('uses CHEX_SCHEMA_DIR env var as fallback', async () => {
            const prev = process.env.CHEX_SCHEMA_DIR
            process.env.CHEX_SCHEMA_DIR = TEST_DIR
            try {
                const result = await validateData(COLLECTION, {
                    id: '10',
                    name: 'Liam',
                    address: { street: 'Env St' }
                })
                expect(result.name).toBe('Liam')
            } finally {
                if (prev === undefined) delete process.env.CHEX_SCHEMA_DIR
                else process.env.CHEX_SCHEMA_DIR = prev
            }
        })

        test('throws when no schemaDir is provided', async () => {
            const prev = process.env.CHEX_SCHEMA_DIR
            // Force chex to receive undefined schemaDir
            delete process.env.CHEX_SCHEMA_DIR
            try {
                await expect(
                    validateData('test-coll', { x: '1' })
                ).rejects.toThrow()
            } finally {
                if (prev !== undefined) process.env.CHEX_SCHEMA_DIR = prev
            }
        })

        test('cache prevents re-reading schema files', async () => {
            const cache = new Map()

            // First call: cache miss, loads from disk
            await validateData(COLLECTION, {
                id: '20', name: 'Olivia', address: { street: 'Cache Test' }
            }, { schemaDir: TEST_DIR, cache })

            expect(cache.has(COLLECTION)).toBe(true)
            const cached = cache.get(COLLECTION)
            expect(cached).toBeDefined()
            expect(cached.id).toBe('^[0-9]+$')

            // Second call: cache hit
            const result = await validateData(COLLECTION, {
                id: '21', name: 'Peter', address: { street: 'Cache Hit' }
            }, { schemaDir: TEST_DIR, cache })
            expect(result.name).toBe('Peter')
        })
    })

    describe('validateData() — edge cases', () => {
        test('rejects nested object with extra fields', async () => {
            await expect(
                validateData(COLLECTION, {
                    id: '30',
                    name: 'Quinn',
                    address: {
                        street: 'Extra St',
                        zipcode: '90210' // not in schema
                    }
                }, { schemaDir: TEST_DIR })
            ).rejects.toThrow()
        })

        test('stringifies numeric values before regex test', async () => {
            const result = await validateData(COLLECTION, {
                id: 50,
                name: 'Ruby',
                age: 25,
                address: { street: 'Number Rd' }
            }, { schemaDir: TEST_DIR })
            expect(result.id).toBe(50)
            expect(result.age).toBe(25)
        })

        test('empty collection name is rejected', async () => {
            await expect(
                validateData('', {}, { schemaDir: TEST_DIR })
            ).rejects.toBeInstanceOf(InvalidNameError)
        })
    })

    describe('FYLO CHEX_SCHEMA_DIR sync (via Fylo class)', () => {
        test('Fylo constructor syncs FYLO_SCHEMA_DIR → CHEX_SCHEMA_DIR', async () => {
            const prevFylo = process.env.FYLO_SCHEMA_DIR
            const prevChex = process.env.CHEX_SCHEMA_DIR
            delete process.env.CHEX_SCHEMA_DIR
            process.env.FYLO_SCHEMA_DIR = TEST_DIR

            try {
                // Dynamic import to trigger constructor side-effect
                const { default: Fylo } = await import('../../src/api/fylo.js')
                new Fylo()
                expect(process.env.CHEX_SCHEMA_DIR).toBe(TEST_DIR)

                // Now chex should work without explicit schemaDir
                const result = await validateData(COLLECTION, {
                    id: '60',
                    name: 'Sync Test',
                    address: { street: 'Sync St' }
                })
                expect(result.name).toBe('Sync Test')
            } finally {
                if (prevFylo === undefined) delete process.env.FYLO_SCHEMA_DIR
                else process.env.FYLO_SCHEMA_DIR = prevFylo
                if (prevChex === undefined) delete process.env.CHEX_SCHEMA_DIR
                else process.env.CHEX_SCHEMA_DIR = prevChex
            }
        })
    })
})
