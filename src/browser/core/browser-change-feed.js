/**
 * @typedef {import('./types.js').BrowserEvent} BrowserEvent
 * @typedef {{ id: string, kind: 'put' | 'delete' }} BrowserChange
 * @typedef {{ changes: BrowserChange[], cursor: string, more: boolean }} BrowserChangePage
 * @typedef {{ sequence: number, id: string, kind: 'put' | 'delete' }} BrowserChangeEntry
 * @typedef {{ lineage: string, sequence: number, id: string }} BrowserChangePosition
 */

/**
 * The change feed of the browser engine's collections (ADR 0024), read from
 * a collection's event journal: every document whose newest change follows a
 * cursor, once, oldest change first. A cursor names the journal's first event,
 * so a collection dropped and created again refuses an old cursor.
 */
export class BrowserChangeFeed {
    static PREFIX = 'fch1'
    static NO_LINEAGE = '0'
    static PARTS = 4
    static DEFAULT_LIMIT = 256
    static MAX_LIMIT = 1000

    /**
     * @param {string} journal
     * @param {string | null | undefined} after
     * @param {number | undefined} limit
     * @returns {BrowserChangePage}
     */
    static page(journal, after, limit) {
        const size = BrowserChangeFeed.#limit(limit)
        const events = BrowserChangeFeed.#events(journal)
        const lineage = BrowserChangeFeed.#lineage(events)
        const position = BrowserChangeFeed.#position(after, lineage)
        BrowserChangeFeed.#requireCurrent(position, lineage, events.length)
        const later = BrowserChangeFeed.#newest(events).filter((entry) =>
            BrowserChangeFeed.#follows(entry, position)
        )
        const page = later.slice(0, size)
        return {
            changes: page.map((entry) => ({ id: entry.id, kind: entry.kind })),
            cursor: BrowserChangeFeed.#cursor(lineage, page.at(-1), position),
            more: later.length > size
        }
    }

    /** @param {number | undefined} limit @returns {number} */
    static #limit(limit) {
        const size = limit ?? BrowserChangeFeed.DEFAULT_LIMIT
        if (Number.isInteger(size) && size >= 1 && size <= BrowserChangeFeed.MAX_LIMIT) return size
        throw BrowserChangeFeed.#error(
            'EBADREQUEST',
            `limit must be an integer from 1 to ${BrowserChangeFeed.MAX_LIMIT}`
        )
    }

    /** @param {string} journal @returns {BrowserEvent[]} */
    static #events(journal) {
        /** @type {BrowserEvent[]} */
        const events = []
        for (const line of journal.split('\n')) {
            const event = BrowserChangeFeed.#parse(line)
            if (event) events.push(event)
        }
        return events
    }

    /** @param {string} line @returns {BrowserEvent | null} */
    static #parse(line) {
        try {
            return JSON.parse(line)
        } catch {
            return null
        }
    }

    /** @param {BrowserEvent[]} events @returns {string} */
    static #lineage(events) {
        const first = events[0]
        if (!first) return BrowserChangeFeed.NO_LINEAGE
        return `${first.ts}-${first.id}`
    }

    /**
     * @param {string | null | undefined} after
     * @param {string} lineage
     * @returns {BrowserChangePosition}
     */
    static #position(after, lineage) {
        if (after === null || after === undefined) return { lineage, sequence: 0, id: '' }
        if (typeof after !== 'string') {
            throw BrowserChangeFeed.#error('EBADREQUEST', 'after must be a change cursor or null')
        }
        return BrowserChangeFeed.#decode(after)
    }

    /** @param {string} text @returns {BrowserChangePosition} */
    static #decode(text) {
        const parts = text.split('.')
        const [prefix, lineage, sequence] = parts
        const id = parts.slice(3).join('.')
        const valid =
            parts.length >= BrowserChangeFeed.PARTS &&
            prefix === BrowserChangeFeed.PREFIX &&
            lineage !== '' &&
            /^\d+$/.test(sequence)
        if (!valid) {
            throw BrowserChangeFeed.#error('EINVALIDCURSOR', 'after is not a change cursor')
        }
        return { lineage, sequence: Number(sequence), id }
    }

    /**
     * @param {BrowserChangePosition} position
     * @param {string} lineage
     * @param {number} length
     */
    static #requireCurrent(position, lineage, length) {
        if (BrowserChangeFeed.#fromStart(position)) return
        if (position.lineage === lineage && position.sequence <= length) return
        throw BrowserChangeFeed.#error(
            'ECURSOR_EXPIRED',
            'the change cursor is from before the collection was created again; read the collection again and continue from after: null'
        )
    }

    /** @param {BrowserChangePosition} position */
    static #fromStart(position) {
        return position.lineage === BrowserChangeFeed.NO_LINEAGE && position.sequence === 0
    }

    /** @param {BrowserEvent[]} events @returns {BrowserChangeEntry[]} */
    static #newest(events) {
        /** @type {Map<string, BrowserChangeEntry>} */
        const newest = new Map()
        events.forEach((event, index) => {
            newest.delete(event.id)
            newest.set(event.id, BrowserChangeFeed.#entry(event, index + 1))
        })
        return [...newest.values()]
    }

    /** @param {BrowserEvent} event @param {number} sequence @returns {BrowserChangeEntry} */
    static #entry(event, sequence) {
        const kind = event.action === 'delete' ? 'delete' : 'put'
        return { sequence, id: event.id, kind }
    }

    /** @param {BrowserChangeEntry} entry @param {BrowserChangePosition} position */
    static #follows(entry, position) {
        return entry.sequence > position.sequence
    }

    /**
     * @param {string} lineage
     * @param {BrowserChangeEntry | undefined} last
     * @param {BrowserChangePosition} position
     * @returns {string}
     */
    static #cursor(lineage, last, position) {
        const sequence = last?.sequence ?? position.sequence
        const id = last?.id ?? position.id
        return `${BrowserChangeFeed.PREFIX}.${lineage}.${sequence}.${id}`
    }

    /** @param {string} code @param {string} message */
    static #error(code, message) {
        /** @type {Error & { code?: string }} */
        const error = new Error(message)
        error.code = code
        return error
    }
}
