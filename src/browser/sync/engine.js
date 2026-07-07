/**
 * @fileoverview Local-first sync engine.
 *
 * Wraps a local (OPFS/memory) browser FYLO client and reconciles it with a
 * backend `fylo serve` over REST. Reads and writes always hit the local store
 * first (so the app works fully offline); a background loop pushes local writes
 * to the backend (`syncPush`, document-level three-way merge) and pulls remote
 * writes via the changes feed (fetch-streamed SSE, JSON-poll fallback). When the
 * backend can't be reached (health ping fails) the local store is the store and
 * writes queue for the next reconnect.
 */

const HEALTH = '/v1/health'
const EXEC = '/v1/exec'

/**
 * @typedef {object} SyncOptions
 * @property {string} [serverUrl]   Backend base URL. Omit ⇒ pure local (offline) mode.
 * @property {string} [token]       Bearer token for the backend.
 * @property {number} [pingMs]      Connectivity poll interval (default 3000).
 * @property {number} [batchMs]     Push debounce window (default 50).
 * @property {(fn: () => void, ms: number) => any} [setInterval]
 * @property {(fn: () => void, ms: number) => any} [setTimeout]
 * @property {typeof fetch} [fetch]
 */

export class SyncEngine {
    /**
     * @param {any} local  A BrowserFyloClient (speaks the machine protocol via `request`).
     * @param {SyncOptions} [options]
     */
    constructor(local, options = {}) {
        this.local = local
        this.serverUrl = options.serverUrl ? options.serverUrl.replace(/\/$/, '') : undefined
        this.token = options.token
        this.pingMs = options.pingMs ?? 3000
        this.batchMs = options.batchMs ?? 50
        this._fetch = options.fetch ?? globalThis.fetch?.bind(globalThis)
        this._setInterval = options.setInterval ?? globalThis.setInterval?.bind(globalThis)
        this._setTimeout = options.setTimeout ?? globalThis.setTimeout?.bind(globalThis)
        this._flushTimer = null
        this.online = false
        /** @type {any[]} pending push changes */
        this.queue = []
        /** @type {Map<string, number>} `${collection}/${id}` → last-synced updatedAt */
        this.base = new Map()
        /** @type {Map<string, number>} collection → last-pulled journal offset */
        this.offsets = new Map()
        /** @type {Set<string>} subscribed collections */
        this.subscribed = new Set()
        /** @type {Map<string, AbortController>} active pull streams */
        this.pulls = new Map()
        this._applyingRemote = false
        this._pingTimer = null
        /** @type {Set<string>} collections ensured to exist locally */
        this._ensured = new Set()
    }

    /** @param {string} collection */
    async _ensureLocal(collection) {
        if (this._ensured.has(collection)) return
        this._ensured.add(collection)
        try {
            await this.local.request({ op: 'createCollection', collection })
        } catch {
            // already exists
        }
    }

    /** @param {Record<string, string>} [extra] */
    _headers(extra) {
        const headers = { ...extra }
        if (this.token) headers.authorization = `Bearer ${this.token}`
        return headers
    }

    /** True when the backend health endpoint responds. */
    async ping() {
        if (!this.serverUrl || !this._fetch) return false
        try {
            const res = await this._fetch(this.serverUrl + HEALTH, { headers: this._headers() })
            return res.ok
        } catch {
            return false
        }
    }

    /** Begin connectivity polling and, when online, flush + resume streams. */
    async start() {
        await this._checkConnectivity()
        if (this._setInterval) {
            this._pingTimer = this._setInterval(() => this._checkConnectivity(), this.pingMs)
        }
    }

    async _checkConnectivity() {
        const was = this.online
        this.online = await this.ping()
        if (this.online && !was) {
            await this._flush()
            for (const collection of this.subscribed) this._openPull(collection)
        }
    }

    /** Run one raw machine op against the backend; resolves with `result`. */
    /** @param {Record<string, any>} op @returns {Promise<any>} */
    async _exec(op) {
        const res = await this._fetch(this.serverUrl + EXEC, {
            method: 'POST',
            headers: this._headers({ 'content-type': 'application/json' }),
            body: JSON.stringify(op)
        })
        const json = await res.json()
        if (!json.ok) throw new Error(json.error?.message ?? 'sync error')
        return json.result
    }

    /**
     * Record a local write for eventual push. No-op while applying remote changes
     * (so pulled writes are not echoed back).
     * @param {string} collection @param {string} id
     * @param {Record<string, any> | null} doc  null ⇒ delete
     */
    capture(collection, id, doc) {
        if (this._applyingRemote) return
        const key = `${collection}/${id}`
        const change = {
            collection,
            id,
            doc: doc ?? undefined,
            deleted: doc === null,
            baseUpdatedAt: this.base.get(key),
            clientUpdatedAt: Date.now()
        }
        // Coalesce rapid edits to the same doc into a single pending change
        // (last write wins), keeping the base version we last synced from.
        const existing = this.queue.findIndex((c) => `${c.collection}/${c.id}` === key)
        if (existing !== -1) {
            change.baseUpdatedAt = this.queue[existing].baseUpdatedAt
            this.queue[existing] = change
        } else {
            this.queue.push(change)
        }
        if (this.online) this._scheduleFlush()
    }

    /** Debounce pushes so a burst of writes becomes one syncPush per collection. */
    _scheduleFlush() {
        if (this._flushTimer || !this._setTimeout) return
        this._flushTimer = this._setTimeout(() => {
            this._flushTimer = null
            void this._flush()
        }, this.batchMs)
    }

    async _flush() {
        if (!this.online || !this.serverUrl || this.queue.length === 0) return
        /** @type {Map<string, any[]>} */
        const byCollection = new Map()
        const pending = this.queue.splice(0)
        for (const change of pending) {
            const list = byCollection.get(change.collection) ?? []
            list.push(change)
            byCollection.set(change.collection, list)
        }
        for (const [collection, changes] of byCollection) {
            try {
                const result = await this._exec({ op: 'syncPush', collection, changes })
                await this._applyResults(collection, result)
            } catch {
                // backend unreachable mid-flush — requeue and go offline
                this.queue.unshift(...changes)
                this.online = false
                return
            }
        }
    }

    /** Fast-forward the local store to the backend's authoritative post-merge state. */
    /** @param {string} collection @param {any} result */
    async _applyResults(collection, result) {
        this._applyingRemote = true
        try {
            for (const [id, record] of Object.entries(result.results ?? {})) {
                await this._applyDoc(collection, id, record.doc)
                if (record.updatedAt != null) this.base.set(`${collection}/${id}`, record.updatedAt)
            }
        } finally {
            this._applyingRemote = false
        }
        if (typeof result.offset === 'number') this.offsets.set(collection, result.offset)
    }

    /** Write one doc (or delete when doc is null) into the local store. */
    /** @param {string} collection @param {string} id @param {Record<string, any> | null} doc */
    async _applyDoc(collection, id, doc) {
        await this._ensureLocal(collection)
        try {
            if (doc === null) {
                await this.local.request({ op: 'delDoc', collection, id })
            } else {
                await this.local.request({ op: 'putData', collection, data: { [id]: doc } })
            }
        } catch {
            // best-effort local materialization
        }
    }

    /** Start pulling a collection's remote changes into the local store. */
    /** @param {string} collection */
    subscribe(collection) {
        this.subscribed.add(collection)
        if (this.online) this._openPull(collection)
    }

    /** @param {string} collection */
    _openPull(collection) {
        this.pulls.get(collection)?.abort()
        const controller = new AbortController()
        this.pulls.set(collection, controller)
        void this._pullLoop(collection, controller.signal)
    }

    /** @param {string} collection @param {AbortSignal} signal */
    async _pullLoop(collection, signal) {
        while (!signal.aborted && this.online) {
            const since = this.offsets.get(collection) ?? 0
            try {
                const url = `${this.serverUrl}/v1/${encodeURIComponent(collection)}/events?since=${since}`
                const res = await this._fetch(url, {
                    headers: this._headers({ accept: 'text/event-stream' }),
                    signal
                })
                if (!res.ok || !res.body) throw new Error('stream failed')
                await this._consumeSSE(collection, res.body, signal)
            } catch {
                if (signal.aborted) return
                // fall back to a single JSON poll (ignore its errors so a failed
                // poll doesn't kill the loop), then retry after a beat.
                try {
                    await this._pollOnce(collection)
                } catch {
                    // transient fallback-poll failure; retry on the next iteration
                }
                // Abortable sleep: stop() aborts the signal and clears the timer.
                // The listener is removed whether we wake by timer or by abort, so
                // it can't accumulate across retries.
                await new Promise((resolve) => {
                    /** @type {ReturnType<typeof setTimeout>} */
                    let timer
                    const onAbort = () => {
                        clearTimeout(timer)
                        resolve(undefined)
                    }
                    timer = setTimeout(() => {
                        signal.removeEventListener('abort', onAbort)
                        resolve(undefined)
                    }, this.pingMs)
                    signal.addEventListener('abort', onAbort, { once: true })
                })
            }
        }
    }

    /** @param {string} collection @param {ReadableStream<Uint8Array>} body @param {AbortSignal} signal */
    async _consumeSSE(collection, body, signal) {
        const reader = body.getReader()
        const decoder = new TextDecoder()
        let buffer = ''
        while (!signal.aborted) {
            const { value, done } = await reader.read()
            if (done) break
            buffer += decoder.decode(value, { stream: true })
            let sep
            while ((sep = buffer.indexOf('\n\n')) !== -1) {
                const frame = buffer.slice(0, sep)
                buffer = buffer.slice(sep + 2)
                const line = frame.split('\n').find((l) => l.startsWith('data:'))
                if (line) await this._applyRemoteBatch(collection, JSON.parse(line.slice(5).trim()))
            }
        }
    }

    /** @param {string} collection */
    async _pollOnce(collection) {
        const since = this.offsets.get(collection) ?? 0
        const url = `${this.serverUrl}/v1/${encodeURIComponent(collection)}/events?since=${since}`
        const res = await this._fetch(url, { headers: this._headers() })
        const json = await res.json()
        if (json.ok) await this._applyRemoteBatch(collection, json.result)
    }

    /** @param {string} collection @param {any} batch */
    async _applyRemoteBatch(collection, batch) {
        this._applyingRemote = true
        try {
            for (const event of batch.events ?? []) {
                const doc = event.action === 'delete' ? null : (event.doc ?? null)
                await this._applyDoc(collection, event.id, doc)
                const updatedAt = event.updatedAt ?? event.ts
                if (updatedAt != null) this.base.set(`${collection}/${event.id}`, updatedAt)
            }
        } finally {
            this._applyingRemote = false
        }
        if (typeof batch.offset === 'number') this.offsets.set(collection, batch.offset)
    }

    /** Stop all timers and streams. */
    stop() {
        if (this._pingTimer) clearInterval(this._pingTimer)
        if (this._flushTimer) clearTimeout(this._flushTimer)
        for (const controller of this.pulls.values()) controller.abort()
        this.pulls.clear()
    }
}
