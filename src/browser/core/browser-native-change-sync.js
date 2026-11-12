import { BrowserPath } from './browser-path.js'

/**
 * @typedef {import('./filesystem.js').FyloFilesystem} FyloFilesystem
 * @typedef {import('./types.js').BrowserEvent} BrowserEvent
 * @typedef {import('./browser-event-bus.js').BrowserEventBus} BrowserEventBus
 * @typedef {import('./documents.js').BrowserDocuments} BrowserDocuments
 * @typedef {{ generation: number, state: string, log?: { length: number } }} NativeGeneration
 * @typedef {{ kind: 'put' | 'delete', version: number | null }} JournalEntry
 * @typedef {{ id: string, mtime: number }} StoredFile
 */

/**
 * Brings a collection's event journal up to date with the writes the native
 * engine made to the same root (ADR 0024, section 4). Only the JavaScript
 * engine writes the journal. The native engine publishes a new generation in
 * `.fylo-transactions/.collections/<collection>/state.json` on every commit,
 * so when that generation differs from the one the journal last saw, the
 * documents are compared with the journal and a `put` or `delete` event is
 * added for every document whose state differs. A scan that a native commit
 * overlaps is thrown away and run again, so it adds nothing twice and misses
 * nothing.
 */
export class BrowserNativeChangeSync {
    static STABLE = 'stable'
    static ATTEMPTS = 8
    static RETRY_DELAY_MS = 25
    static NAMESPACE = '.collections'
    static BUSY_CODE = 'ENATIVE_CONCURRENT_WRITE'

    /**
     * @param {FyloFilesystem} filesystem
     * @param {string} root
     * @param {BrowserEventBus} events
     * @param {BrowserDocuments} documents
     */
    constructor(filesystem, root, events, documents) {
        this.fs = filesystem
        this.root = root
        this.events = events
        this.documents = documents
    }

    /** @param {string} collection @returns {Promise<void>} */
    async synchronize(collection) {
        for (let attempt = 1; attempt < BrowserNativeChangeSync.ATTEMPTS; attempt += 1) {
            if (await this.#trySynchronize(collection)) return
            await BrowserNativeChangeSync.#wait(attempt)
        }
        await this.#lastAttempt(collection)
    }

    /** @param {string} collection @returns {Promise<void>} */
    async #lastAttempt(collection) {
        if (await this.#trySynchronize(collection)) return
        throw BrowserNativeChangeSync.#busy(collection)
    }

    /** @param {string} collection @returns {Promise<boolean>} */
    async #trySynchronize(collection) {
        const before = await this.#generation(collection)
        if (before === null) return true
        return await this.#synchronizeStable(collection, before)
    }

    /**
     * @param {string} collection
     * @param {NativeGeneration} before
     * @returns {Promise<boolean>}
     */
    async #synchronizeStable(collection, before) {
        if (!(await this.#settled(collection, before))) return false
        return await this.#synchronizeUnseen(collection, before)
    }

    /**
     * @param {string} collection
     * @param {NativeGeneration} before
     * @returns {Promise<boolean>}
     */
    async #synchronizeUnseen(collection, before) {
        if ((await this.#recordedGeneration(collection)) === before.generation) return true
        return await this.#scan(collection, before)
    }

    /**
     * @param {string} collection
     * @param {NativeGeneration} before
     * @returns {Promise<boolean>}
     */
    async #scan(collection, before) {
        const view = BrowserNativeChangeSync.#journalView(await this.events.journal(collection))
        const live = await this.#withTimes(await this.documents.listDocFiles(collection))
        const deleted = await this.#withTimes(await this.documents.listDeletedDocFiles(collection))
        const found = BrowserNativeChangeSync.#differences(view, live, deleted)
        const after = await this.#generation(collection)
        return await this.#commitIfUnchanged(collection, before, after, found)
    }

    /**
     * @param {string} collection
     * @param {NativeGeneration} before
     * @param {NativeGeneration | null} after
     * @param {BrowserEvent[]} found
     * @returns {Promise<boolean>}
     */
    async #commitIfUnchanged(collection, before, after, found) {
        if (!BrowserNativeChangeSync.#sameGeneration(before, after)) return false
        await this.events.append(collection, found)
        await this.fs.mkdir(BrowserPath.dirname(this.#recordPath(collection)), { recursive: true })
        await this.fs.writeText(
            this.#recordPath(collection),
            JSON.stringify({ generation: before.generation })
        )
        return true
    }

    /**
     * @param {NativeGeneration} before
     * @param {NativeGeneration | null} after
     * @returns {boolean}
     */
    static #sameGeneration(before, after) {
        return (
            after !== null &&
            after.state === BrowserNativeChangeSync.STABLE &&
            after.generation === before.generation &&
            after.log?.length === before.log?.length
        )
    }

    /**
     * A stable generation whose write-ahead log is longer or shorter than it
     * records belongs to a native writer that stopped between its commit and
     * its publication; its documents may not be applied yet.
     *
     * @param {string} collection
     * @param {NativeGeneration} generation
     * @returns {Promise<boolean>}
     */
    async #settled(collection, generation) {
        if (generation.state !== BrowserNativeChangeSync.STABLE) return false
        return await this.#logMatches(collection, generation)
    }

    /**
     * @param {string} collection
     * @param {NativeGeneration} generation
     * @returns {Promise<boolean>}
     */
    async #logMatches(collection, generation) {
        if (!generation.log) return true
        const log = BrowserPath.join(this.#nativeRoot(collection), 'log', 'segment.log')
        return (await this.#sizeOrNull(log)) === generation.log.length
    }

    /** @param {string} path @returns {Promise<number | null>} */
    async #sizeOrNull(path) {
        if (!(await this.fs.exists(path))) return null
        return await this.fs.size(path)
    }

    /** @param {string} collection @returns {Promise<NativeGeneration | null>} */
    async #generation(collection) {
        const path = BrowserPath.join(this.#nativeRoot(collection), 'state.json')
        if (!(await this.fs.exists(path))) return null
        return BrowserNativeChangeSync.#parseGeneration(await this.#readOrEmpty(path))
    }

    /**
     * `state.json` is replaced by rename on every native commit, so it can
     * vanish between the check and the read. An empty text reads as a
     * generation that is still being written, which is tried again.
     *
     * @param {string} path
     * @returns {Promise<string>}
     */
    async #readOrEmpty(path) {
        try {
            return await this.fs.readText(path)
        } catch {
            return ''
        }
    }

    /** @param {string} text @returns {NativeGeneration} */
    static #parseGeneration(text) {
        try {
            return JSON.parse(text)
        } catch {
            return { generation: -1, state: 'writing' }
        }
    }

    /** @param {string} collection @returns {Promise<number | null>} */
    async #recordedGeneration(collection) {
        const path = this.#recordPath(collection)
        if (!(await this.fs.exists(path))) return null
        return BrowserNativeChangeSync.#parseGeneration(await this.fs.readText(path)).generation
    }

    /** @param {string} collection @returns {string} */
    #recordPath(collection) {
        return BrowserPath.join(
            BrowserPath.dirname(this.events.journalPath(collection)),
            `${collection}.native.json`
        )
    }

    /** @param {string} collection @returns {string} */
    #nativeRoot(collection) {
        return BrowserPath.join(
            this.root,
            '.fylo-transactions',
            BrowserNativeChangeSync.NAMESPACE,
            collection
        )
    }

    /**
     * @param {import('./documents.js').BucketedFile[]} files
     * @returns {Promise<StoredFile[]>}
     */
    async #withTimes(files) {
        /** @type {{ id: string, mtime: number | null }[]} */
        const stored = []
        for (const file of files) stored.push({ id: file.id, mtime: await this.#time(file.path) })
        return stored.filter(BrowserNativeChangeSync.#hasTime)
    }

    /**
     * @param {{ id: string, mtime: number | null }} file
     * @returns {file is StoredFile}
     */
    static #hasTime(file) {
        return file.mtime !== null
    }

    /**
     * A native writer can move a file after it was listed. The file then
     * counts as absent; the generation check after the scan throws such a
     * scan away.
     *
     * @param {string} path
     * @returns {Promise<number | null>}
     */
    async #time(path) {
        try {
            return await this.fs.mtimeMs(path)
        } catch {
            return null
        }
    }

    /**
     * The newest state of every document the journal names. A `meta` event
     * keeps the document's version, because it changes no document file.
     *
     * @param {string} journal
     * @returns {Map<string, JournalEntry>}
     */
    static #journalView(journal) {
        /** @type {Map<string, JournalEntry>} */
        const view = new Map()
        for (const event of BrowserNativeChangeSync.#events(journal)) {
            view.set(event.id, BrowserNativeChangeSync.#entry(event, view.get(event.id)))
        }
        return view
    }

    /** @param {string} journal @returns {BrowserEvent[]} */
    static #events(journal) {
        return journal
            .split('\n')
            .map(BrowserNativeChangeSync.#parseEvent)
            .filter((event) => event !== null)
    }

    /** @param {string} line @returns {BrowserEvent | null} */
    static #parseEvent(line) {
        try {
            return JSON.parse(line)
        } catch {
            return null
        }
    }

    /**
     * @param {BrowserEvent} event
     * @param {JournalEntry | undefined} previous
     * @returns {JournalEntry}
     */
    static #entry(event, previous) {
        if (event.action === 'meta') return previous ?? { kind: 'put', version: null }
        if (event.action === 'delete') return { kind: 'delete', version: null }
        return { kind: 'put', version: event.ts }
    }

    /**
     * Events for the documents whose stored state differs from the journal,
     * oldest file first, then by id, so the same files always give the same
     * events.
     *
     * @param {Map<string, JournalEntry>} view
     * @param {StoredFile[]} live
     * @param {StoredFile[]} deleted
     * @returns {BrowserEvent[]}
     */
    static #differences(view, live, deleted) {
        const liveIds = new Set(live.map((file) => file.id))
        const puts = live.filter((file) =>
            BrowserNativeChangeSync.#changedLive(view.get(file.id), file)
        )
        const deletes = deleted.filter(
            (file) => !liveIds.has(file.id) && view.get(file.id)?.kind !== 'delete'
        )
        const vanished = BrowserNativeChangeSync.#vanished(view, liveIds, deleted)
        return [
            ...puts.map((file) => BrowserNativeChangeSync.#event('insert', file)),
            ...[...deletes, ...vanished].map((file) =>
                BrowserNativeChangeSync.#event('delete', file)
            )
        ].sort(BrowserNativeChangeSync.#byTimeThenId)
    }

    /**
     * @param {JournalEntry | undefined} entry
     * @param {StoredFile} file
     * @returns {boolean}
     */
    static #changedLive(entry, file) {
        return entry?.kind !== 'put' || entry.version !== file.mtime
    }

    /**
     * Documents the journal holds as live that are now in neither folder.
     *
     * @param {Map<string, JournalEntry>} view
     * @param {Set<string>} liveIds
     * @param {StoredFile[]} deleted
     * @returns {StoredFile[]}
     */
    static #vanished(view, liveIds, deleted) {
        const deletedIds = new Set(deleted.map((file) => file.id))
        return [...view.entries()]
            .filter(
                ([id, entry]) => entry.kind === 'put' && !liveIds.has(id) && !deletedIds.has(id)
            )
            .map(([id]) => ({ id, mtime: 0 }))
    }

    /**
     * @param {'insert' | 'delete'} action
     * @param {StoredFile} file
     * @returns {BrowserEvent}
     */
    static #event(action, file) {
        return { ts: file.mtime, action, id: file.id, source: 'native' }
    }

    /** @param {BrowserEvent} left @param {BrowserEvent} right @returns {number} */
    static #byTimeThenId(left, right) {
        return left.ts - right.ts || BrowserNativeChangeSync.#compareText(left.id, right.id)
    }

    /** @param {string} left @param {string} right @returns {number} */
    static #compareText(left, right) {
        if (left === right) return 0
        return left < right ? -1 : 1
    }

    /** @param {number} attempt @returns {Promise<void>} */
    static #wait(attempt) {
        return new Promise((resolve) =>
            setTimeout(resolve, BrowserNativeChangeSync.RETRY_DELAY_MS * attempt)
        )
    }

    /** @param {string} collection @returns {Error & { code: string }} */
    static #busy(collection) {
        const error = /** @type {Error & { code: string }} */ (
            new Error(
                `a native writer kept changing collection ${collection}; read the changes again`
            )
        )
        error.code = BrowserNativeChangeSync.BUSY_CODE
        return error
    }
}
