import { join } from './path.js'

/**
 * @typedef {import('./filesystem.js').FyloFilesystem} FyloFilesystem
 * @typedef {import('./types.js').BrowserEvent} BrowserEvent
 */

/**
 * Append-only event journal for FYLO browser. It persists NDJSON using the same
 * collection event path shape as the Bun filesystem engine and also provides
 * in-process pub/sub for direct, worker-less browser usage.
 */
export class BrowserEventBus {
    /**
     * @param {FyloFilesystem} fs
     * @param {(collection: string) => string} rootForCollection
     */
    constructor(fs, rootForCollection) {
        this.fs = fs
        this.rootForCollection = rootForCollection
        /** @type {EventTarget} */
        this.target = new EventTarget()
    }

    /** @param {string} collection @returns {string} */
    journalPath(collection) {
        return join(this.rootForCollection(collection), 'events', `${collection}.ndjson`)
    }

    /**
     * @param {string} collection
     * @param {BrowserEvent} event
     * @returns {Promise<void>}
     */
    async publish(collection, event) {
        await this.fs.appendText(this.journalPath(collection), `${JSON.stringify(event)}\n`)
        this.target.dispatchEvent(new CustomEvent(collection, { detail: event }))
    }

    /**
     * @param {string} collection
     * @param {(event: BrowserEvent) => void} listener
     * @returns {() => void}
     */
    subscribe(collection, listener) {
        /** @param {Event} event */
        const handler = (event) => {
            listener(/** @type {CustomEvent<BrowserEvent>} */ (event).detail)
        }
        this.target.addEventListener(collection, handler)
        return () => this.target.removeEventListener(collection, handler)
    }

    /**
     * @param {string} collection
     * @returns {AsyncGenerator<BrowserEvent, void, unknown>}
     */
    async *listen(collection) {
        /** @type {BrowserEvent[]} */
        const queue = []
        /** @type {() => void} */
        let wake = () => {}
        const unsubscribe = this.subscribe(collection, (event) => {
            queue.push(event)
            wake()
        })
        try {
            while (true) {
                if (queue.length === 0) {
                    await new Promise((resolve) => {
                        wake = () => resolve(undefined)
                    })
                }
                while (queue.length > 0) yield /** @type {BrowserEvent} */ (queue.shift())
            }
        } finally {
            unsubscribe()
        }
    }
}
