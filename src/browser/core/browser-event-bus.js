import { BrowserPath } from './browser-path.js'
import { BrowserEventStream } from './browser-event-stream.js'

/**
 * @typedef {import('./filesystem.js').FyloFilesystem} FyloFilesystem
 * @typedef {import('./types.js').BrowserEvent} BrowserEvent
 */

export class BrowserEventBus {
    /**
     * @param {FyloFilesystem} filesystem
     * @param {(collection: string) => string} rootForCollection
     */
    constructor(filesystem, rootForCollection) {
        this.fs = filesystem
        this.rootForCollection = rootForCollection
        /** @type {EventTarget} */
        this.target = new EventTarget()
    }

    /** @param {string} collection @returns {string} */
    journalPath(collection) {
        return BrowserPath.join(
            this.rootForCollection(collection),
            'events',
            `${collection}.ndjson`
        )
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
    listen(collection) {
        const stream = new BrowserEventStream(this.subscribe.bind(this, collection))
        return stream.iterate()
    }
}
