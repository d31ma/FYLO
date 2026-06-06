import { BrowserCore } from '../core/engine.js'
import { runBrowserRequest } from '../core/protocol.js'
import { createMemoryFilesystem } from '../core/memory-filesystem.js'
import { createOpfsFilesystem } from '../opfs-filesystem.js'

/**
 * @typedef {import('../core/types.js').BrowserCoreOptions} BrowserCoreOptions
 * @typedef {import('../core/types.js').BrowserRequest} BrowserRequest
 * @typedef {import('../core/types.js').BrowserEvent} BrowserEvent
 * @typedef {{ id?: string, namespace?: string, type?: string, collection?: string, request?: BrowserRequest, storage?: 'memory' | 'opfs', root?: string, worm?: BrowserCoreOptions['worm'] }} WorkerEnvelope
 */

/**
 * Worker-side multiplexer. One browser worker can host many FYLO namespaces,
 * each with its own filesystem/core instance.
 */
export class FyloWorkerRuntime {
    constructor() {
        /** @type {Map<string, BrowserCore>} */
        this.cores = new Map()
        /** @type {Map<string, Set<MessagePort | { postMessage: (message: any) => void }>>} */
        this.subscriptions = new Map()
        /** @type {Map<string, () => void>} */
        this.coreSubscriptions = new Map()
    }

    /**
     * @param {WorkerEnvelope} envelope
     * @returns {BrowserCore}
     */
    core(envelope) {
        const namespace = envelope.namespace ?? 'fylo'
        const existing = this.cores.get(namespace)
        if (existing) return existing
        const fs =
            envelope.storage === 'memory'
                ? createMemoryFilesystem()
                : createOpfsFilesystem({ namespace })
        const core = new BrowserCore({
            fs,
            root: envelope.root ?? '/',
            worm: envelope.worm
        })
        this.cores.set(namespace, core)
        return core
    }

    /**
     * @param {WorkerEnvelope} envelope
     * @returns {Promise<BrowserCore>}
     */
    async readyCore(envelope) {
        const core = this.core(envelope)
        return core
    }

    /**
     * @param {MessagePort | { postMessage: (message: any) => void }} port
     * @param {WorkerEnvelope} envelope
     * @returns {Promise<void>}
     */
    async dispatch(port, envelope) {
        if (envelope.type === 'subscribe') {
            await this.subscribe(port, envelope)
            this.post(port, { id: envelope.id, ok: true, result: true })
            return
        }
        if (envelope.type === 'unsubscribe') {
            this.unsubscribe(port, envelope)
            this.post(port, { id: envelope.id, ok: true, result: true })
            return
        }
        const request = envelope.request
        if (!request) throw new Error('FYLO worker request envelope is missing request')
        const response = await runBrowserRequest(await this.readyCore(envelope), request)
        this.post(port, { id: envelope.id, ...response })
    }

    /**
     * @param {MessagePort | { postMessage: (message: any) => void }} port
     * @param {WorkerEnvelope} envelope
     */
    async subscribe(port, envelope) {
        const namespace = envelope.namespace ?? 'fylo'
        const collection = envelope.collection
        if (!collection) throw new Error('FYLO worker subscribe requires collection')
        const key = `${namespace}:${collection}`
        let ports = this.subscriptions.get(key)
        if (!ports) {
            ports = new Set()
            this.subscriptions.set(key, ports)
        }
        ports.add(port)
        if (!this.coreSubscriptions.has(key)) {
            const unsubscribe = (await this.readyCore(envelope)).subscribe(
                collection,
                /**
                 * @param {BrowserEvent} event
                 */
                (event) => {
                    this.broadcast(namespace, collection, event)
                }
            )
            this.coreSubscriptions.set(key, unsubscribe)
        }
    }

    /**
     * @param {MessagePort | { postMessage: (message: any) => void }} port
     * @param {WorkerEnvelope} envelope
     */
    unsubscribe(port, envelope) {
        const namespace = envelope.namespace ?? 'fylo'
        const collection = envelope.collection
        if (!collection) return
        const key = `${namespace}:${collection}`
        const ports = this.subscriptions.get(key)
        ports?.delete(port)
        if (ports && ports.size === 0) {
            this.subscriptions.delete(key)
            this.coreSubscriptions.get(key)?.()
            this.coreSubscriptions.delete(key)
        }
    }

    /**
     * @param {string} namespace
     * @param {string} collection
     * @param {BrowserEvent} event
     */
    broadcast(namespace, collection, event) {
        const ports = this.subscriptions.get(`${namespace}:${collection}`)
        if (!ports) return
        for (const port of ports) {
            this.post(port, {
                type: 'event',
                namespace,
                collection,
                event
            })
        }
    }

    /**
     * @param {MessagePort | { postMessage: (message: any) => void }} port
     * @param {Record<string, any>} message
     */
    post(port, message) {
        port.postMessage(message)
    }
}

/**
 * @param {FyloWorkerRuntime} runtime
 * @param {MessagePort | { postMessage: (message: any) => void }} port
 * @param {unknown} message
 * @returns {Promise<void>}
 */
export async function handleWorkerMessage(runtime, port, message) {
    try {
        await runtime.dispatch(port, /** @type {WorkerEnvelope} */ (message))
    } catch (error) {
        const failure = /** @type {Error} */ (error)
        runtime.post(port, {
            id: /** @type {WorkerEnvelope} */ (message)?.id,
            ok: false,
            error: {
                name: failure.name || 'Error',
                message: failure.message || 'Unknown error'
            }
        })
    }
}
