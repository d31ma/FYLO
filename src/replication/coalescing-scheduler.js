/**
 * Runs one task at a time and remembers at most one request that arrives while
 * the task is active. Closing cancels that pending request and waits for the
 * active task, so resource owners can release descriptors safely afterwards.
 */
export class CoalescingScheduler {
    /**
     * @param {() => Promise<void>} task
     * @param {{ intervalMs?: number, minimumIntervalMs?: number, beforeInterval?: () => void, onError?: (error: unknown) => void }} [options]
     */
    constructor(task, options = {}) {
        this.task = task
        this.intervalMs = options.intervalMs
        this.minimumIntervalMs = options.minimumIntervalMs ?? 1_000
        this.beforeInterval = options.beforeInterval
        this.onError = options.onError
        if (this.intervalMs !== undefined) {
            if (
                !Number.isSafeInteger(this.intervalMs) ||
                this.intervalMs < this.minimumIntervalMs
            ) {
                throw new TypeError(
                    `reconcileIntervalMs must be a safe integer >= ${this.minimumIntervalMs}`
                )
            }
        }
    }

    state = 'open'
    pending = false
    /** @type {Promise<void> | null} */
    active = null
    /** @type {ReturnType<typeof setInterval> | undefined} */
    timer = undefined

    start() {
        if (this.state !== 'open' || this.timer || this.intervalMs === undefined) return
        this.timer = setInterval(() => {
            this.beforeInterval?.()
            void this.trigger().catch((error) => this.onError?.(error))
        }, this.intervalMs)
        this.timer.unref?.()
    }

    /** @returns {Promise<void>} */
    trigger() {
        if (this.state !== 'open') {
            return Promise.reject(new Error(`S3 backup scheduler is ${this.state}`))
        }
        if (this.active) {
            this.pending = true
            return this.active
        }
        this.active = this.run()
        return this.active
    }

    async run() {
        try {
            do {
                this.pending = false
                await this.task()
            } while (this.pending && this.state === 'open')
        } finally {
            this.pending = false
            this.active = null
        }
    }

    /** Stop accepting work, cancel the pending pass, and drain the active pass. */
    async close() {
        if (this.state === 'closed') return
        this.state = 'closing'
        this.pending = false
        if (this.timer) clearInterval(this.timer)
        this.timer = undefined
        try {
            await this.active
        } finally {
            this.state = 'closed'
        }
    }
}
