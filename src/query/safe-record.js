/**
 * Creates a dictionary without Object.prototype. User-controlled keys such as
 * `__proto__`, `constructor`, and `prototype` are ordinary data keys here.
 *
 * @template T
 * @returns {Record<string, T>}
 */
export function safeRecord() {
    return Object.create(null)
}

/**
 * Copies own enumerable properties into a null-prototype dictionary.
 *
 * @template T
 * @param {Record<string, T>} source
 * @returns {Record<string, T>}
 */
export function copySafeRecord(source) {
    const target = safeRecord()
    for (const [key, value] of Object.entries(source)) target[key] = value
    return target
}

/**
 * Recursively copies JSON-shaped data into arrays and null-prototype records.
 * This is useful at import boundaries where parsed object keys are untrusted.
 *
 * @param {unknown} value
 * @returns {any}
 */
export function copySafeJson(value) {
    if (Array.isArray(value)) return value.map(copySafeJson)
    if (typeof value !== 'object' || value === null) return value
    const target = safeRecord()
    for (const [key, nested] of Object.entries(value)) target[key] = copySafeJson(nested)
    return target
}
