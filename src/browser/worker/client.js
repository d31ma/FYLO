/**
 * @typedef {import('../core/types.js').BrowserCoreOptions} BrowserCoreOptions
 * @typedef {import('../core/types.js').BrowserRequest} BrowserRequest
 * @typedef {import('../core/types.js').BrowserEvent} BrowserEvent
 * @typedef {import('../core/types.js').BrowserSuccessResponse | import('../core/types.js').BrowserErrorResponse} BrowserResponse
 */

/**
 * @typedef {object} FyloWorkerClientOptions
 * @property {string} namespace
 * @property {'memory' | 'opfs'} storage
 * @property {string=} root
 * @property {BrowserCoreOptions['worm']=} worm
 */

export class FyloWorkerClient {
    /**
     * @param {MessagePort | Worker} port
     * @param {FyloWorkerClientOptions} options
     */
    constructor(port, options) {
        this.port = port
        this.options = options
        this.sequence = 0
        /** @type {Map<string, { resolve: (value: any) => void, reject: (error: Error) => void }>} */
        this.pending = new Map()
        /** @type {Map<string, Set<(event: BrowserEvent) => void>>} */
        this.listeners = new Map()
        const messagePort = /** @type {{ onmessage: ((event: MessageEvent) => void) | null }} */ (
            /** @type {unknown} */ (this.port)
        )
        messagePort.onmessage = (event) => this.receive(event.data)
        if ('start' in this.port) this.port.start()
    }

    /**
     * @param {unknown} message
     */
    receive(message) {
        const envelope = /** @type {Record<string, any>} */ (message)
        if (envelope.type === 'event') {
            const key = String(envelope.collection)
            for (const listener of this.listeners.get(key) ?? []) listener(envelope.event)
            return
        }
        const id = String(envelope.id ?? '')
        const pending = this.pending.get(id)
        if (!pending) return
        this.pending.delete(id)
        if (envelope.ok === false) {
            pending.reject(new Error(envelope.error?.message ?? 'FYLO worker request failed'))
            return
        }
        pending.resolve(envelope)
    }

    /** @returns {string} */
    nextId() {
        this.sequence += 1
        return `${Date.now()}-${this.sequence}`
    }

    /**
     * @param {Record<string, any>} envelope
     * @returns {Promise<any>}
     */
    send(envelope) {
        const id = this.nextId()
        const payload = {
            id,
            namespace: this.options.namespace,
            storage: this.options.storage,
            root: this.options.root,
            worm: this.options.worm,
            ...envelope
        }
        const promise = new Promise((resolve, reject) => {
            this.pending.set(id, { resolve, reject })
        })
        this.port.postMessage(payload)
        return promise
    }

    /**
     * @param {BrowserRequest} request
     * @returns {Promise<unknown>}
     */
    async request(request) {
        const response = /** @type {BrowserResponse} */ (await this.envelope(request))
        if (!response.ok) throw new Error(response.error.message)
        return response.result
    }

    /**
     * @param {BrowserRequest} request
     * @returns {Promise<BrowserResponse>}
     */
    async envelope(request) {
        return /** @type {BrowserResponse} */ (await this.send({ request }))
    }

    /**
     * @param {string} collection
     * @param {(event: BrowserEvent) => void} listener
     * @returns {() => void}
     */
    subscribe(collection, listener) {
        let listeners = this.listeners.get(collection)
        if (!listeners) {
            listeners = new Set()
            this.listeners.set(collection, listeners)
            void this.send({ type: 'subscribe', collection })
        }
        listeners.add(listener)
        return () => {
            const current = this.listeners.get(collection)
            current?.delete(listener)
            if (current && current.size === 0) {
                this.listeners.delete(collection)
                void this.send({ type: 'unsubscribe', collection })
            }
        }
    }

    /** @returns {Promise<void>} */
    async close() {
        if ('terminate' in this.port) this.port.terminate()
        if ('close' in this.port) this.port.close()
    }
}

/**
 * @param {FyloWorkerClientOptions} options
 * @returns {FyloWorkerClient}
 */
export function createWorkerClient(options) {
    if (typeof SharedWorker !== 'undefined') {
        const worker = new SharedWorker(new URL('./shared.js', import.meta.url), {
            type: 'module',
            name: 'fylo-browser'
        })
        return new FyloWorkerClient(worker.port, options)
    }
    if (typeof Worker !== 'undefined') {
        const worker = new Worker(new URL('./dedicated.js', import.meta.url), { type: 'module' })
        return new FyloWorkerClient(worker, options)
    }
    throw new Error('FYLO browser workers are not available in this runtime')
}
