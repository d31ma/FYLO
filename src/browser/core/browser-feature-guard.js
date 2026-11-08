export class BrowserFeatureGuard {
    static CODE = 'EBROWSER_UNSUPPORTED'
    static ENCRYPTION_OPTIONS = ['$encrypted', 'encryption', 'encrypted', 'cipher']
    static DOCUMENT_KIND = 'document'

    /**
     * @param {unknown} options
     * @returns {void}
     */
    static requireDocumentCollection(options) {
        const record = BrowserFeatureGuard.#record(options)
        BrowserFeatureGuard.#rejectOtherKind(record.kind)
        BrowserFeatureGuard.#rejectSchema(record)
        BrowserFeatureGuard.#rejectEncryption(record)
    }

    /**
     * @param {unknown} options
     * @returns {void}
     */
    static requirePlainClientOptions(options) {
        BrowserFeatureGuard.#rejectEncryption(BrowserFeatureGuard.#record(options))
    }

    /**
     * @param {unknown} data
     * @returns {void}
     */
    static requireDocumentData(data) {
        BrowserFeatureGuard.#rejectBinary(data)
        BrowserFeatureGuard.#rejectBinaryValues(BrowserFeatureGuard.#record(data))
    }

    /**
     * @param {unknown} batch
     * @returns {void}
     */
    static requireDocumentBatch(batch) {
        BrowserFeatureGuard.#rejectBinaryItems(BrowserFeatureGuard.#list(batch))
    }

    /**
     * @param {unknown} value
     * @returns {unknown[]}
     */
    static #list(value) {
        if (!Array.isArray(value)) return []
        return value
    }

    /**
     * @param {unknown} value
     * @returns {Record<string, unknown>}
     */
    static #record(value) {
        if (value === null || typeof value !== 'object') return {}
        return /** @type {Record<string, unknown>} */ (value)
    }

    /** @param {unknown} kind */
    static #rejectOtherKind(kind) {
        if (kind === undefined || kind === BrowserFeatureGuard.DOCUMENT_KIND) return
        throw BrowserFeatureGuard.#unsupported(
            `The FYLO browser engine stores document collections only; kind "${String(kind)}" (file buckets) needs the native engine`
        )
    }

    /** @param {Record<string, unknown>} record */
    static #rejectSchema(record) {
        if (!Object.hasOwn(record, 'schema')) return
        throw BrowserFeatureGuard.#unsupported(
            'The FYLO browser engine does not support collection schemas; use the native engine for schema validation'
        )
    }

    /** @param {Record<string, unknown>} record */
    static #rejectEncryption(record) {
        const requested = BrowserFeatureGuard.ENCRYPTION_OPTIONS.filter((name) =>
            Object.hasOwn(record, name)
        )
        if (requested.length === 0) return
        throw BrowserFeatureGuard.#unsupported(
            `The FYLO browser engine does not support encryption (${requested.join(', ')}); nothing was stored`
        )
    }

    /** @param {unknown[]} items */
    static #rejectBinaryItems(items) {
        for (const item of items) BrowserFeatureGuard.requireDocumentData(item)
    }

    /** @param {Record<string, unknown>} record */
    static #rejectBinaryValues(record) {
        for (const value of Object.values(record)) BrowserFeatureGuard.#rejectBinary(value)
    }

    /** @param {unknown} value */
    static #rejectBinary(value) {
        if (!BrowserFeatureGuard.#isBinary(value)) return
        throw BrowserFeatureGuard.#unsupported(
            'The FYLO browser engine does not store Blob, File, or URL values; document collections take plain objects'
        )
    }

    /**
     * @param {unknown} value
     * @returns {boolean}
     */
    static #isBinary(value) {
        if (typeof Blob !== 'undefined' && value instanceof Blob) return true
        return typeof URL !== 'undefined' && value instanceof URL
    }

    /**
     * @param {string} message
     * @returns {Error & { code: string }}
     */
    static #unsupported(message) {
        const error = /** @type {Error & { code: string }} */ (new Error(message))
        error.name = 'BrowserUnsupportedError'
        error.code = BrowserFeatureGuard.CODE
        return error
    }
}
