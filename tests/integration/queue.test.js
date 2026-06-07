import { afterAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import path from 'node:path'
import Fylo, { LocalQueue, consume, publish } from '../../src/index.js'
import { createTestRoot } from '../helpers/root.js'

const roots = []

async function createRoot(prefix) {
    const root = await createTestRoot(prefix)
    roots.push(root)
    return root
}

afterAll(async () => {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })))
})

describe('local queue', () => {
    test('queueing is opt-in on Fylo instances', async () => {
        const root = await createRoot('fylo-queue-off-')
        const fylo = new Fylo(root)
        expect(fylo.queue).toBeUndefined()
        expect(
            await Bun.file(
                path.join(root, '.collections', 'users', 'index', 'manifest.json')
            ).exists()
        ).toBe(true)
    })

    test('mirrors document events into collection action topics', async () => {
        const root = await createRoot('fylo-queue-events-')
        const fylo = new Fylo(root, { queue: true })
        const seeded = await fylo['users'].get('4V6329YC0F2').once()
        expect(seeded['4V6329YC0F2'].name).toBe('Ada Lovelace')
        await fylo['queue-users'].create()

        const id = await fylo['queue-users'].put({ name: 'Alice' })
        const seen = []
        const result = await fylo.queue.drain(
            'queue-users.insert',
            { group: 'audit', maxMessages: 10 },
            async (message) => {
                seen.push(message)
            }
        )

        expect(result.processed).toBe(1)
        expect(seen).toHaveLength(1)
        expect(seen[0].payload.collection).toBe('queue-users')
        expect(seen[0].payload.action).toBe('insert')
        expect(seen[0].payload.id).toBe(id)
        expect(seen[0].payload.doc).toEqual({ name: 'Alice' })
    })

    test('consume helper registers class methods without decorator syntax', async () => {
        const root = await createRoot('fylo-queue-consume-')
        const queue = new LocalQueue({ root })

        class AuditConsumer {
            seen = []

            async record(message, context) {
                this.seen.push(message.payload.kind)
                context.ack()
            }
        }

        consume('audit.created', { group: 'audit-service', autoAck: false })(
            AuditConsumer.prototype,
            'record'
        )

        await queue.publish('audit.created', { kind: 'created' })
        const consumer = new AuditConsumer()
        const result = await queue.drainRegistered(consumer)

        expect(result.processed).toBe(1)
        expect(consumer.seen).toEqual(['created'])
    })

    test('retries failed messages before advancing the checkpoint', async () => {
        const root = await createRoot('fylo-queue-retry-')
        const queue = new LocalQueue({ root })
        await queue.publish('retry.topic', { id: 1 })

        let attempts = 0
        const first = await queue.drain(
            'retry.topic',
            { group: 'workers', maxRetries: 3 },
            async () => {
                attempts++
                throw new Error('try again')
            }
        )
        const second = await queue.drain(
            'retry.topic',
            { group: 'workers', maxRetries: 3 },
            async (message) => {
                attempts++
                expect(message.attempt).toBe(2)
            }
        )

        expect(first.processed).toBe(0)
        expect(first.failed).toBe(1)
        expect(second.processed).toBe(1)
        expect(attempts).toBe(2)
    })

    test('dead-letters messages that exceed max retries', async () => {
        const root = await createRoot('fylo-queue-dlq-')
        const queue = new LocalQueue({ root })
        await queue.publish('poison.topic', { id: 1 })

        const result = await queue.drain(
            'poison.topic',
            { group: 'workers', maxRetries: 1 },
            async () => {
                throw new Error('poison')
            }
        )
        const dlq = await Bun.file(
            path.join(root, '.queue', 'dlq', `${encodeURIComponent('poison.topic')}.ndjson`)
        ).text()

        expect(result.failed).toBe(1)
        expect(result.deadLettered).toBe(1)
        expect(dlq).toContain('poison')

        const replay = await queue.drain('poison.topic', { group: 'workers' }, async () => {
            throw new Error('should not replay')
        })
        expect(replay.processed).toBe(0)
        expect(replay.failed).toBe(0)
    })

    test('publish helper publishes a method return value while preserving the return', async () => {
        const root = await createRoot('fylo-queue-publish-')
        const queue = new LocalQueue({ root })

        class UserService {
            constructor(queue) {
                this.queue = queue
            }

            createUser(name) {
                return { name }
            }
        }

        publish('users.created')(UserService.prototype, 'createUser')

        const service = new UserService(queue)
        const returned = await service.createUser('Ada')
        const seen = []
        await queue.drain('users.created', { group: 'audit' }, async (message) => {
            seen.push(message.payload)
        })

        expect(returned).toEqual({ name: 'Ada' })
        expect(seen).toEqual([{ name: 'Ada' }])
    })

    test('publish helper supports async functions and custom payload mapping', async () => {
        const root = await createRoot('fylo-queue-publish-map-')
        const queue = new LocalQueue({ root })
        const createUser = publish('users.mapped', {
            queue,
            map: (result, args) => ({
                userId: result.id,
                requestedName: args[0]
            })
        })(async (name) => ({ id: 'u1', name }))

        const returned = await createUser('Grace')
        const seen = []
        await queue.drain('users.mapped', { group: 'audit' }, async (message) => {
            seen.push(message.payload)
        })

        expect(returned).toEqual({ id: 'u1', name: 'Grace' })
        expect(seen).toEqual([{ userId: 'u1', requestedName: 'Grace' }])
    })

    test('publish helper skips undefined returns by default', async () => {
        const root = await createRoot('fylo-queue-publish-skip-')
        const queue = new LocalQueue({ root })
        const command = publish('commands.undefined', { queue })(() => undefined)

        expect(await command()).toBeUndefined()
        const result = await queue.drain('commands.undefined', { group: 'audit' }, async () => {})

        expect(result.processed).toBe(0)
    })
})
