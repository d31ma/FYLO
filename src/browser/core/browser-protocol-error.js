import { CollectionNotFoundError } from '../../core/collection.js'

export class BrowserProtocolError {
    /**
     * @param {unknown} error
     * @returns {Error & { code?: string }}
     */
    static from(error) {
        if (!error || typeof error !== 'object') {
            return new Error('Unknown browser protocol error')
        }
        const record = /** @type {Record<string, unknown>} */ (error)
        if (record.code === 'FYLO_COLLECTION_NOT_FOUND') {
            return BrowserProtocolError.collectionNotFound(record)
        }
        return BrowserProtocolError.failure(record)
    }

    /**
     * @param {Record<string, unknown>} record
     * @returns {CollectionNotFoundError}
     */
    static collectionNotFound(record) {
        const message = typeof record.message === 'string' ? record.message : ''
        return new CollectionNotFoundError(message.replace(/^Collection not found: /, ''))
    }

    /**
     * @param {Record<string, unknown>} record
     * @returns {Error & { code?: string }}
     */
    static failure(record) {
        /** @type {Error & { code?: string }} */
        const failure = new Error(
            typeof record.message === 'string' ? record.message : 'Request failed'
        )
        failure.name = typeof record.name === 'string' ? record.name : 'Error'
        if (typeof record.code === 'string') failure.code = record.code
        return failure
    }
}
