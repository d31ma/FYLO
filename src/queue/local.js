import path from 'node:path'
import { mkdir, open } from 'node:fs/promises'
import { tryAcquireFileLock, tryReleaseFileLock } from '../storage/fs-lock.js'
import { writeDurable } from '../storage/durable.js'

/**
 * @typedef {object} QueueMessage
 * @property {string} id
 * @property {string} topic
 * @property {number} ts
 * @property {Record<string, any>} payload
 */

/**
 * @typedef {object} QueueDelivery
 * @property {string} id
 * @property {string} topic
 * @property {number} ts
 * @property {number} attempt
 * @property {Record<string, any>} payload
 */

/**
 * @typedef {object} QueueConsumeOptions
 * @property {string} group
 * @property {number=} maxMessages
 * @property {number=} maxRetries
 * @property {boolean=} autoAck
 * @property {number=} leaseMs
 */

/**
 * @typedef {object} QueueDrainResult
 * @property {number} processed
 * @property {number} failed
 * @property {number} deadLettered
 * @property {number} skipped
 */

/**
 * @typedef {(message: QueueDelivery, context: QueueMessageContext) => Promise<void> | void} QueueHandler
 */

/**
 * @typedef {object} QueuePublishOptions
 * @property {LocalQueue | ((self: any, args: any[], result: any) => LocalQueue | undefined)=} queue
 * @property {(result: any, args: any[], self: any) => Record<string, any> | undefined | null=} map
 * @property {boolean=} skipUndefined
 */

/**
 * @typedef {{ topic: string, methodName: string | symbol, options: Omit<QueueConsumeOptions, 'group'> & { group?: string } }} QueueConsumerRegistration
 */

const DEFAULT_MAX_RETRIES = 3
const DEFAULT_LEASE_MS = 30_000
const CONSUMERS = Symbol.for('fylo.queue.consumers')

/**
 * @param {string} value
 * @returns {string}
 */
function encodeName(value) {
    return encodeURIComponent(value)
}

/**
 * @param {string} topic
 */
function validateTopic(topic) {
    if (typeof topic !== 'string' || topic.trim().length === 0) {
        throw new Error('Queue topic must be a non-empty string')
    }
    if (topic.includes('/') || topic.includes('\\')) {
        throw new Error('Queue topic must not contain path separators')
    }
}

/**
 * @param {string} group
 */
function validateGroup(group) {
    if (typeof group !== 'string' || group.trim().length === 0) {
        throw new Error('Queue consumer group must be a non-empty string')
    }
    if (group.includes('/') || group.includes('\\')) {
        throw new Error('Queue consumer group must not contain path separators')
    }
}

/**
 * @param {unknown} error
 * @returns {string}
 */
function errorMessage(error) {
    return error instanceof Error ? error.message : String(error)
}

/**
 * @param {Function} ctor
 * @param {QueueConsumerRegistration} registration
 */
function addConsumerRegistration(ctor, registration) {
    const target = /** @type {any} */ (ctor)
    const existing = /** @type {QueueConsumerRegistration[]} */ (target[CONSUMERS] ?? [])
    const duplicate = existing.some(
        (item) =>
            item.topic === registration.topic &&
            item.methodName === registration.methodName &&
            item.options.group === registration.options.group
    )
    if (duplicate) return
    target[CONSUMERS] = [...existing, registration]
}

/**
 * @param {unknown} value
 * @returns {Record<string, any>}
 */
function normalizePublishPayload(value) {
    if (value && typeof value === 'object' && !Array.isArray(value)) {
        return /** @type {Record<string, any>} */ (value)
    }
    return { value }
}

/**
 * @param {QueuePublishOptions} options
 * @param {any} self
 * @param {any[]} args
 * @param {any} result
 * @returns {LocalQueue}
 */
function resolvePublishQueue(options, self, args, result) {
    const source =
        typeof options.queue === 'function' ? options.queue(self, args, result) : options.queue
    const queue = source ?? self?.queue
    if (!queue || typeof queue.publish !== 'function') {
        throw new Error('@publish requires a LocalQueue via options.queue or this.queue')
    }
    return queue
}

/**
 * @param {string} topic
 * @param {QueuePublishOptions} options
 * @param {any} self
 * @param {any[]} args
 * @param {any} result
 * @returns {Promise<any>}
 */
async function publishReturnValue(topic, options, self, args, result) {
    if (result === undefined && options.skipUndefined !== false) return result
    const mapped = options.map ? options.map(result, args, self) : normalizePublishPayload(result)
    if (mapped == null) return result
    await resolvePublishQueue(options, self, args, result).publish(topic, mapped)
    return result
}

/**
 * @param {string} text
 * @returns {string[]}
 */
function completeLines(text) {
    if (!text) return []
    const complete = text.endsWith('\n') ? text : text.slice(0, text.lastIndexOf('\n') + 1)
    return complete.split('\n').filter(Boolean)
}

/**
 * @param {string} target
 * @returns {Promise<string>}
 */
async function readTextIfExists(target) {
    try {
        return await Bun.file(target).text()
    } catch (err) {
        const error = /** @type {NodeJS.ErrnoException} */ (err)
        if (error.code === 'ENOENT') return ''
        throw err
    }
}

/**
 * @param {string} target
 * @returns {Promise<Record<string, any>>}
 */
async function readJsonIfExists(target) {
    const text = await readTextIfExists(target)
    if (!text) return {}
    return JSON.parse(text)
}

/**
 * @param {string} target
 * @param {string} data
 * @returns {Promise<void>}
 */
async function appendDurable(target, data) {
    await mkdir(path.dirname(target), { recursive: true })
    const handle = await open(target, 'a')
    try {
        await handle.writeFile(data)
        await handle.sync()
    } finally {
        await handle.close()
    }
}

/**
 * Per-message acknowledgement context passed to queue consumers.
 */
export class QueueMessageContext {
    /** @type {boolean} */
    acked = false
    /** @type {unknown} */
    nackReason

    /** Marks the message as successfully handled when auto-ack is disabled. */
    ack() {
        this.acked = true
    }

    /** @param {unknown} [reason] */
    nack(reason = new Error('Message was nacked')) {
        this.nackReason = reason
    }
}

/**
 * Durable local queue backed by append-only topic files, consumer checkpoints,
 * advisory leases, and dead-letter queues under the FYLO root.
 */
export class LocalQueue {
    /** @type {string} */
    root

    /**
     * @param {{ root: string }} options
     */
    constructor(options) {
        this.root = options.root
    }

    /** @returns {string} */
    queueRoot() {
        return path.join(this.root, '.queue')
    }

    /** @param {string} topic @returns {string} */
    topicPath(topic) {
        validateTopic(topic)
        return path.join(this.queueRoot(), 'topics', `${encodeName(topic)}.ndjson`)
    }

    /** @param {string} topic @returns {string} */
    dlqPath(topic) {
        validateTopic(topic)
        return path.join(this.queueRoot(), 'dlq', `${encodeName(topic)}.ndjson`)
    }

    /** @param {string} group @param {string} topic @returns {string} */
    checkpointPath(group, topic) {
        validateGroup(group)
        validateTopic(topic)
        return path.join(
            this.queueRoot(),
            'consumers',
            encodeName(group),
            `${encodeName(topic)}.json`
        )
    }

    /** @param {string} group @param {string} topic @returns {string} */
    leasePath(group, topic) {
        validateGroup(group)
        validateTopic(topic)
        return path.join(this.queueRoot(), 'leases', encodeName(group), `${encodeName(topic)}.lock`)
    }

    /**
     * @param {string} topic
     * @param {Record<string, any>} payload
     * @returns {Promise<string>}
     */
    async publish(topic, payload) {
        validateTopic(topic)
        const message = {
            id: Bun.randomUUIDv7(),
            topic,
            ts: Date.now(),
            payload
        }
        await appendDurable(this.topicPath(topic), `${JSON.stringify(message)}\n`)
        return message.id
    }

    /**
     * @param {string} collection
     * @param {{ action: 'insert' | 'delete' | 'meta', id: string, ts: number, doc?: Record<string, any>, meta?: Record<string, any> }} event
     * @returns {Promise<string>}
     */
    async publishCollectionEvent(collection, event) {
        return await this.publish(`${collection}.${event.action}`, {
            collection,
            action: event.action,
            id: event.id,
            ts: event.ts,
            doc: event.doc
        })
    }

    /**
     * @param {string} topic
     * @param {number} position
     * @param {number} maxMessages
     * @returns {Promise<Array<{ message: QueueMessage, nextPosition: number }>>}
     */
    async readTopic(topic, position, maxMessages) {
        const text = await readTextIfExists(this.topicPath(topic))
        if (!text) return []
        const slice = Buffer.from(text).subarray(position).toString('utf8')
        const records = []
        let nextPosition = position
        for (const line of completeLines(slice)) {
            nextPosition += Buffer.byteLength(`${line}\n`)
            records.push({
                message: /** @type {QueueMessage} */ (JSON.parse(line)),
                nextPosition
            })
            if (records.length >= maxMessages) break
        }
        return records
    }

    /**
     * @param {string} group
     * @param {string} topic
     * @returns {Promise<{ position: number, failures: Record<string, { attempts: number, lastError?: string }> }>}
     */
    async readCheckpoint(group, topic) {
        const checkpoint = await readJsonIfExists(this.checkpointPath(group, topic))
        return {
            position: Number(checkpoint.position ?? 0),
            failures:
                checkpoint.failures && typeof checkpoint.failures === 'object'
                    ? checkpoint.failures
                    : {}
        }
    }

    /**
     * @param {string} group
     * @param {string} topic
     * @param {{ position: number, failures: Record<string, { attempts: number, lastError?: string }> }} checkpoint
     */
    async writeCheckpoint(group, topic, checkpoint) {
        await writeDurable(this.checkpointPath(group, topic), `${JSON.stringify(checkpoint)}\n`)
    }

    /**
     * @param {string} group
     * @param {string} topic
     * @param {QueueMessage} message
     * @param {unknown} error
     * @param {number} attempts
     */
    async writeDeadLetter(group, topic, message, error, attempts) {
        await appendDurable(
            this.dlqPath(topic),
            `${JSON.stringify({
                id: Bun.randomUUIDv7(),
                group,
                topic,
                sourceMessage: message,
                attempts,
                error: errorMessage(error),
                ts: Date.now()
            })}\n`
        )
    }

    /**
     * @param {string} topic
     * @param {QueueConsumeOptions} options
     * @param {QueueHandler} handler
     * @returns {Promise<QueueDrainResult>}
     */
    async drain(topic, options, handler) {
        validateTopic(topic)
        validateGroup(options.group)
        const owner = Bun.randomUUIDv7()
        const leaseMs = options.leaseMs ?? DEFAULT_LEASE_MS
        const lockPath = this.leasePath(options.group, topic)
        const acquired = await tryAcquireFileLock(lockPath, owner, {
            ttlMs: leaseMs,
            heartbeat: true
        })
        if (!acquired) return { processed: 0, failed: 0, deadLettered: 0, skipped: 0 }
        try {
            return await this.drainLocked(topic, options, handler)
        } finally {
            await tryReleaseFileLock(lockPath, owner)
        }
    }

    /**
     * @param {string} topic
     * @param {QueueConsumeOptions} options
     * @param {QueueHandler} handler
     * @returns {Promise<QueueDrainResult>}
     */
    async drainLocked(topic, options, handler) {
        const maxMessages = options.maxMessages ?? 100
        const maxRetries = options.maxRetries ?? DEFAULT_MAX_RETRIES
        const autoAck = options.autoAck ?? true
        const checkpoint = await this.readCheckpoint(options.group, topic)
        const records = await this.readTopic(topic, checkpoint.position, maxMessages)
        const result = { processed: 0, failed: 0, deadLettered: 0, skipped: 0 }
        for (const record of records) {
            const failures = checkpoint.failures[record.message.id]
            const attempt = (failures?.attempts ?? 0) + 1
            const context = new QueueMessageContext()
            try {
                await handler(
                    {
                        ...record.message,
                        attempt
                    },
                    context
                )
                if (!autoAck && !context.acked) {
                    throw new Error('Message handler completed without ack()')
                }
                if (context.nackReason !== undefined) throw context.nackReason
                delete checkpoint.failures[record.message.id]
                checkpoint.position = record.nextPosition
                result.processed++
            } catch (err) {
                checkpoint.failures[record.message.id] = {
                    attempts: attempt,
                    lastError: errorMessage(err)
                }
                result.failed++
                if (attempt >= maxRetries) {
                    await this.writeDeadLetter(options.group, topic, record.message, err, attempt)
                    delete checkpoint.failures[record.message.id]
                    checkpoint.position = record.nextPosition
                    result.deadLettered++
                }
                await this.writeCheckpoint(options.group, topic, checkpoint)
                if (attempt < maxRetries) break
                continue
            }
            await this.writeCheckpoint(options.group, topic, checkpoint)
        }
        return result
    }

    /**
     * @param {object} target
     * @returns {QueueConsumerRegistration[]}
     */
    registrationsFor(target) {
        const ctor = /** @type {any} */ (target.constructor)
        return /** @type {QueueConsumerRegistration[]} */ (ctor[CONSUMERS] ?? [])
    }

    /**
     * @param {object} target
     * @param {{ group?: string, maxMessages?: number }} [options]
     * @returns {Promise<QueueDrainResult>}
     */
    async drainRegistered(target, options = {}) {
        const registrations = this.registrationsFor(target)
        const totals = { processed: 0, failed: 0, deadLettered: 0, skipped: 0 }
        for (const registration of registrations) {
            const group = registration.options.group ?? options.group
            if (!group)
                throw new Error(
                    `Missing queue consumer group for ${String(registration.methodName)}`
                )
            const method = /** @type {any} */ (target)[registration.methodName]
            if (typeof method !== 'function') {
                throw new Error(
                    `Registered queue consumer is not a function: ${String(registration.methodName)}`
                )
            }
            const result = await this.drain(
                registration.topic,
                {
                    ...registration.options,
                    group,
                    maxMessages: options.maxMessages ?? registration.options.maxMessages
                },
                method.bind(target)
            )
            totals.processed += result.processed
            totals.failed += result.failed
            totals.deadLettered += result.deadLettered
            totals.skipped += result.skipped
        }
        return totals
    }

    /**
     * @param {object} target
     * @param {{ group?: string, intervalMs?: number, signal?: AbortSignal }} [options]
     * @returns {Promise<void>}
     */
    async runRegistered(target, options = {}) {
        const intervalMs = options.intervalMs ?? 100
        while (!options.signal?.aborted) {
            await this.drainRegistered(target, options)
            await Bun.sleep(intervalMs)
        }
    }
}

/**
 * Decorator-compatible queue consumer registration.
 *
 * Works as a plain JavaScript helper:
 * consume('users.insert', { group: 'email' })(Consumer.prototype, 'send')
 *
 * Also supports modern decorator runtimes that pass `(method, context)`.
 *
 * @param {string} topic
 * @param {Omit<QueueConsumeOptions, 'group'> & { group?: string }} [options]
 * @returns {Function}
 */
export function consume(topic, options = {}) {
    validateTopic(topic)
    /**
     * @param {Function | object} target
     * @param {string | symbol | { kind: string, name: string | symbol, addInitializer: (initializer: (this: object) => void) => void }} [propertyKey]
     */
    return function registerConsumer(target, propertyKey) {
        if (propertyKey && (typeof propertyKey === 'string' || typeof propertyKey === 'symbol')) {
            const ctor = /** @type {any} */ (target.constructor)
            addConsumerRegistration(ctor, { topic, methodName: propertyKey, options })
            return
        }
        if (
            typeof target === 'function' &&
            typeof propertyKey === 'object' &&
            propertyKey?.kind === 'method'
        ) {
            const context = propertyKey
            context.addInitializer(function initializeConsumer() {
                const ctor = /** @type {any} */ (this.constructor)
                addConsumerRegistration(ctor, { topic, methodName: context.name, options })
            })
            return target
        }
        throw new Error('@consume can only be applied to methods')
    }
}

/**
 * Decorator-compatible publisher registration.
 *
 * Plain JavaScript usage:
 * MyService.prototype.create = publish('users.created', { queue })(MyService.prototype.create)
 *
 * Decorator usage:
 * @publish('users.created')
 * async createUser() { return { id: 'u1' } }
 *
 * The wrapped method still returns its original value after publishing.
 *
 * @param {string} topic
 * @param {QueuePublishOptions} [options]
 * @returns {Function}
 */
export function publish(topic, options = {}) {
    validateTopic(topic)
    /**
     * @param {Function} method
     * @returns {Function}
     */
    const wrap = (method) => {
        if (typeof method !== 'function') throw new Error('@publish can only wrap functions')
        /**
         * @this {any}
         * @param {...any} args
         * @returns {Promise<any>}
         */
        return async function publishWrappedMethod(...args) {
            const result = await method.apply(this, args)
            return await publishReturnValue(topic, options, this, args, result)
        }
    }
    /**
     * @param {Function | object} target
     * @param {string | symbol | { kind: string, name: string | symbol }=} propertyKey
     * @param {{ value?: Function }=} descriptor
     * @returns {Function | object | void}
     */
    return function registerPublisher(target, propertyKey, descriptor) {
        if (typeof target === 'function' && propertyKey === undefined) {
            return wrap(target)
        }
        if (descriptor?.value) {
            descriptor.value = wrap(descriptor.value)
            return descriptor
        }
        if (
            typeof target === 'function' &&
            typeof propertyKey === 'object' &&
            propertyKey?.kind === 'method'
        ) {
            return wrap(target)
        }
        if (propertyKey && (typeof propertyKey === 'string' || typeof propertyKey === 'symbol')) {
            const targetObject = /** @type {Record<string | symbol, any>} */ (target)
            const current = targetObject[propertyKey]
            if (typeof current !== 'function') {
                throw new Error('@publish can only wrap methods')
            }
            targetObject[propertyKey] = wrap(current)
            return
        }
        throw new Error('@publish can only be applied to methods or functions')
    }
}
