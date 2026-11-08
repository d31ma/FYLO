export class MetadataRecord {
    static #name = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/
    static #maximumValueBytes = 60 * 1024

    /** @param {Record<string, any>} record */
    static validate(record) {
        this.#checkRecord(record)
        this.#checkEntries(record)
    }

    /** @param {Record<string, any>} record */
    static #checkRecord(record) {
        this.#requireRecord(typeof record === 'object')
        this.#requireRecord(record !== null)
        this.#requireRecord(!Array.isArray(record))
    }

    /** @param {boolean} valid */
    static #requireRecord(valid) {
        if (!valid) throw new Error('Metadata must be a plain object of name/value pairs')
    }

    /** @param {Record<string, any>} record */
    static #checkEntries(record) {
        for (const [name, value] of Object.entries(record)) {
            this.#checkName(name)
            this.#checkValue(value)
        }
    }

    /** @param {string} name */
    static #checkName(name) {
        if (!this.#name.test(name)) {
            throw new Error(
                'Metadata name must be 1-64 characters: letters, digits, ".", "_" or "-", starting with a letter or digit'
            )
        }
    }

    /** @param {any} value */
    static #checkValue(value) {
        if (value !== null) this.#checkEncodedValue(value)
    }

    /** @param {any} value */
    static #checkEncodedValue(value) {
        const encoded = JSON.stringify(value)
        this.#requireJson(encoded)
        this.#checkSize(new TextEncoder().encode(encoded).byteLength)
    }

    /** @param {string | undefined} encoded */
    static #requireJson(encoded) {
        if (encoded === undefined) throw new Error('Metadata values must be JSON-serializable')
    }

    /** @param {number} bytes */
    static #checkSize(bytes) {
        if (bytes > this.#maximumValueBytes) {
            throw new Error('Metadata values must be at most 60 KiB when JSON-encoded')
        }
    }
}
