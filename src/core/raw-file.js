/** @type {Readonly<Record<string, string>>} */
const MIME_BY_EXTENSION = Object.freeze({
    '.avif': 'image/avif',
    '.bin': 'application/octet-stream',
    '.bmp': 'image/bmp',
    '.csv': 'text/csv',
    '.gif': 'image/gif',
    '.gz': 'application/gzip',
    '.html': 'text/html',
    '.jpeg': 'image/jpeg',
    '.jpg': 'image/jpeg',
    '.json': 'application/json',
    '.md': 'text/markdown',
    '.mov': 'video/quicktime',
    '.mp3': 'audio/mpeg',
    '.mp4': 'video/mp4',
    '.pdf': 'application/pdf',
    '.png': 'image/png',
    '.svg': 'image/svg+xml',
    '.tar': 'application/x-tar',
    '.txt': 'text/plain',
    '.wav': 'audio/wav',
    '.webm': 'video/webm',
    '.webp': 'image/webp',
    '.xml': 'application/xml',
    '.zip': 'application/zip'
})

/**
 * @typedef {object} RawFileMetadata
 * @property {string} name
 * @property {string} key
 * @property {string} extension
 * @property {string} contentType
 * @property {number} contentLength
 * @property {string} etag
 * @property {string} checksumSHA256
 * @property {number} createdAt
 * @property {number} lastModified
 */

/**
 * Returns a safe final extension, including the leading dot.
 * @param {string | undefined} name
 * @param {string | undefined} contentType
 * @returns {string}
 */
export function rawFileExtension(name, contentType) {
    const cleanName = String(name ?? '')
        .split(/[?#]/, 1)[0]
        .replaceAll('\\', '/')
    const basename = cleanName.slice(cleanName.lastIndexOf('/') + 1)
    const dot = basename.lastIndexOf('.')
    const candidate = dot > 0 ? basename.slice(dot).toLowerCase() : ''
    if (/^\.[a-z0-9]{1,16}$/.test(candidate)) return candidate
    for (const [extension, type] of Object.entries(MIME_BY_EXTENSION)) {
        if (type === contentType) return extension
    }
    return '.bin'
}

/**
 * @param {string} extension
 * @param {string | undefined} contentType
 * @returns {string}
 */
export function rawFileContentType(extension, contentType) {
    const normalized = String(contentType ?? '')
        .split(';', 1)[0]
        .trim()
        .toLowerCase()
    if (/^[a-z0-9!#$&^_.+-]+\/[a-z0-9!#$&^_.+-]+$/.test(normalized)) return normalized
    return MIME_BY_EXTENSION[extension] ?? 'application/octet-stream'
}

/**
 * @param {string} filename
 * @returns {string | null}
 */
export function rawFileId(filename) {
    const dot = filename.indexOf('.')
    const id = dot === -1 ? filename : filename.slice(0, dot)
    return /^[A-Za-z0-9_-]+$/.test(id) ? id : null
}

/**
 * Resolves a logical object key. `/` and trailing-slash prefixes receive the
 * generated TTID filename so every implicit key remains unique.
 *
 * @param {string | undefined} requestedKey
 * @param {string} id
 * @param {string} extension
 * @returns {string}
 */
export function rawFileKey(requestedKey, id, extension) {
    let key = requestedKey ?? '/'
    if (typeof key !== 'string') throw new TypeError('Raw file key must be a string')
    if (!key.startsWith('/')) key = `/${key}`
    if (/[\u0000-\u001f\u007f\\]/.test(key)) {
        throw new Error('Raw file key contains unsupported characters')
    }
    if (key.endsWith('/')) key += `${id}${extension}`
    if (new TextEncoder().encode(key).byteLength > 1024) {
        throw new Error('Raw file key must not exceed 1024 UTF-8 bytes')
    }
    for (const segment of key.split('/')) {
        if (segment === '') continue
        let decoded
        try {
            decoded = decodeURIComponent(segment)
        } catch {
            throw new Error('Raw file key contains invalid percent encoding')
        }
        if (decoded === '.' || decoded === '..') {
            throw new Error('Raw file key must not contain "." or ".." path segments')
        }
    }
    return key
}

/**
 * @param {string} id
 * @param {string} key
 * @param {string} extension
 * @param {string} contentType
 * @param {number} contentLength
 * @param {string} checksumSHA256
 * @param {number} createdAt
 * @param {number} lastModified
 * @returns {RawFileMetadata}
 */
export function rawFileMetadata(
    id,
    key,
    extension,
    contentType,
    contentLength,
    checksumSHA256,
    createdAt,
    lastModified
) {
    return {
        name: `${id}${extension}`,
        key,
        extension,
        contentType,
        contentLength,
        etag: checksumSHA256,
        checksumSHA256,
        createdAt,
        lastModified
    }
}
