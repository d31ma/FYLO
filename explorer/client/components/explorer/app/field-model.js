// Pure helpers for the Firestore-style field / type / regex editing model.
// Kept out of the component so the class stays focused on state + orchestration.

/** JSON type of a value: null | boolean | number | string | array | object. */
export function valueType(v) {
    if (v === null) return 'null'
    if (Array.isArray(v)) return 'array'
    return typeof v
}

/** String repr of a value for an <input> (object/array as compact JSON). */
export function fieldValue(v) {
    const type = valueType(v)
    if (type === 'null') return ''
    if (type === 'object' || type === 'array') return JSON.stringify(v)
    return String(v)
}

/** Default CHEX-style regex for a field, keyed by its value's JSON type. */
export function defaultRegex(type) {
    return {
        string: '^.*$',
        number: '^-?\\d+(\\.\\d+)?$',
        boolean: '^(true|false)$',
        null: '^null$',
        array: '^\\[.*\\]$',
        object: '^\\{.*\\}$'
    }[type]
}

/** Parse one edit field back to its typed value (throws on bad input). */
export function parseField(f) {
    switch (f.type) {
        case 'number': {
            const n = Number(f.value)
            if (!Number.isFinite(n))
                throw new Error(`Field "${f.key}": "${f.value}" is not a number`)
            return n
        }
        case 'boolean':
            return f.value === 'true'
        case 'null':
            return null
        case 'array': {
            const p = JSON.parse(f.value)
            if (!Array.isArray(p)) throw new Error(`Field "${f.key}": not an array`)
            return p
        }
        case 'object': {
            const p = JSON.parse(f.value)
            if (!p || typeof p !== 'object' || Array.isArray(p))
                throw new Error(`Field "${f.key}": not an object`)
            return p
        }
        default: // string
            return f.value
    }
}
