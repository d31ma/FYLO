/** @typedef {import('./types.js').BrowserEvent} BrowserEvent */

export class BrowserEventStream {
    static #emptyLength = 0
    /** @type {BrowserEvent[]} */
    #queue = []
    /** @type {() => void} */
    #wake = () => {}

    /** @param {(listener: (event: BrowserEvent) => void) => () => void} subscribe */
    constructor(subscribe) {
        this.subscribe = subscribe
    }

    /** @returns {AsyncGenerator<BrowserEvent, void, unknown>} */
    iterate() {
        return this.#withSubscription()
    }

    /** @returns {AsyncGenerator<BrowserEvent, void, unknown>} */
    async *#withSubscription() {
        const unsubscribe = this.subscribe(this.#receive.bind(this))
        try {
            yield* this.#readEvents()
        } finally {
            unsubscribe()
        }
    }

    /** @param {BrowserEvent} event */
    #receive(event) {
        this.#queue.push(event)
        this.#wake()
    }

    /** @returns {AsyncGenerator<BrowserEvent, void, unknown>} */
    async *#readEvents() {
        while (true) {
            await this.#waitForEvent()
            yield* this.#drainQueue()
        }
    }

    /** @returns {Promise<void>} */
    async #waitForEvent() {
        if (this.#queue.length === BrowserEventStream.#emptyLength) {
            await this.#waitForPublication()
        }
    }

    /** @returns {Promise<void>} */
    #waitForPublication() {
        return new Promise((resolve) => {
            this.#wake = () => resolve(undefined)
        })
    }

    /** @returns {AsyncGenerator<BrowserEvent, void, unknown>} */
    async *#drainQueue() {
        while (this.#queue.length > BrowserEventStream.#emptyLength) {
            yield /** @type {BrowserEvent} */ (this.#queue.shift())
        }
    }
}
