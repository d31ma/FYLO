import { lookup } from 'node:dns/promises'
import { isIP } from 'node:net'
import { checkServerIdentity as tlsCheckServerIdentity } from 'node:tls'

/**
 * @typedef {object} ImportBulkDataOptions
 * @property {number=} limit
 * @property {number=} maxBytes
 * @property {string[]=} allowedProtocols
 * @property {string[]=} allowedHosts
 * @property {boolean=} allowPrivateNetwork
 */

/** Default maximum response body size for bulk imports: 50 MiB. */
export const DEFAULT_IMPORT_MAX_BYTES = 50 * 1024 * 1024

/** @param {number | ImportBulkDataOptions | undefined} limitOrOptions @returns {Required<Omit<ImportBulkDataOptions, 'limit' | 'allowedHosts'>> & Pick<ImportBulkDataOptions, 'limit' | 'allowedHosts'>} */
export function normalizeImportOptions(limitOrOptions) {
    const options = typeof limitOrOptions === 'number' ? { limit: limitOrOptions } : limitOrOptions
    return {
        limit: options?.limit,
        maxBytes: options?.maxBytes ?? DEFAULT_IMPORT_MAX_BYTES,
        allowedProtocols: options?.allowedProtocols ?? ['https:', 'http:', 'data:'],
        allowedHosts: options?.allowedHosts,
        allowPrivateNetwork: options?.allowPrivateNetwork ?? false
    }
}

/** @param {number} first @param {number} second @returns {boolean} */
export function isPrivateIPv4(first, second) {
    return (
        first === 0 ||
        first === 10 ||
        first === 127 ||
        (first === 169 && second === 254) ||
        (first === 172 && second >= 16 && second <= 31) ||
        (first === 192 && second === 168) ||
        (first === 100 && second >= 64 && second <= 127)
    )
}

/** @param {string} ipv6 @returns {number[] | null} */
export function expandIPv6(ipv6) {
    const trailingV4 = ipv6.match(/(\d+\.\d+\.\d+\.\d+)$/)
    let head = ipv6
    /** @type {number[]} */
    const tail = []
    if (trailingV4) {
        const [firstOctet, secondOctet, thirdOctet, fourthOctet] = trailingV4[1]
            .split('.')
            .map((part) => Number(part))
        tail.push((firstOctet << 8) | secondOctet, (thirdOctet << 8) | fourthOctet)
        head = ipv6.slice(0, ipv6.length - trailingV4[1].length).replace(/:$/, '')
        if (head === '') head = '::'
    }
    const parts = head.split('::')
    if (parts.length > 2) return null
    const leftRaw = parts[0] ? parts[0].split(':') : []
    const rightRaw = parts.length === 2 ? (parts[1] ? parts[1].split(':') : []) : parts[0] ? [] : []
    const left = leftRaw.map((segment) => Number.parseInt(segment, 16))
    const right = rightRaw.map((segment) => Number.parseInt(segment, 16))
    const missing = 8 - tail.length - left.length - right.length
    if (missing < 0) return null
    const middle = parts.length === 2 ? new Array(missing).fill(0) : []
    if (parts.length === 1 && left.length + tail.length !== 8) return null
    const segments = [...left, ...middle, ...right, ...tail]
    if (segments.length !== 8 || segments.some((s) => Number.isNaN(s) || s < 0 || s > 0xffff))
        return null
    return segments
}

/** @param {string} address @returns {boolean} */
export function isPrivateAddress(address) {
    const normalized = address
        .toLowerCase()
        .replace(/^\[|\]$/g, '')
        .split('%')[0]
    if (isIP(normalized) === 4) {
        const [first = 0, second = 0] = normalized.split('.').map((part) => Number(part))
        return isPrivateIPv4(first, second)
    }
    if (isIP(normalized) === 6) {
        const segments = expandIPv6(normalized)
        if (!segments) return false
        if (segments.every((s) => s === 0)) return true
        if (segments.slice(0, 7).every((s) => s === 0) && segments[7] === 1) return true
        if (segments.slice(0, 5).every((s) => s === 0) && segments[5] === 0xffff) {
            const first = segments[6] >> 8
            const second = segments[6] & 0xff
            return isPrivateIPv4(first, second)
        }
        const firstSegment = segments[0]
        return (firstSegment & 0xfe00) === 0xfc00 || (firstSegment & 0xffc0) === 0xfe80
    }
    return false
}

/** @param {string} hostname @param {string[] | undefined} allowedHosts @returns {boolean} */
export function hostAllowed(hostname, allowedHosts) {
    if (!allowedHosts?.length) return true
    const host = hostname.toLowerCase()
    return allowedHosts.some((allowed) => {
        const candidate = allowed.toLowerCase()
        return host === candidate || host.endsWith(`.${candidate}`)
    })
}

/**
 * Strips userinfo, query, and fragment from a URL so it can be emitted in
 * observability events without leaking pre-signed URL params, basic-auth
 * credentials, or query-string secrets.
 *
 * @param {URL | string} url
 * @returns {string}
 */
export function redactImportUrl(url) {
    try {
        const redacted = new URL(url instanceof URL ? url.toString() : url)
        redacted.username = ''
        redacted.password = ''
        redacted.search = ''
        redacted.hash = ''
        return redacted.toString()
    } catch {
        return '[unparseable-url]'
    }
}

/**
 * @param {URL} url
 * @param {ReturnType<typeof normalizeImportOptions>} options
 * @returns {Promise<{ pinnedUrls: URL[], serverName: string } | null>}
 */
export async function assertImportUrlAllowed(url, options) {
    if (!options.allowedProtocols.includes(url.protocol))
        throw new Error(`Import URL protocol is not allowed: ${url.protocol}`)
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return null
    if (!hostAllowed(url.hostname, options.allowedHosts))
        throw new Error(`Import URL host is not allowed: ${url.hostname}`)
    const hostname = url.hostname.toLowerCase().replace(/^\[|\]$/g, '')
    if (!options.allowPrivateNetwork) {
        if (hostname === 'localhost' || hostname.endsWith('.localhost'))
            throw new Error(`Import URL resolves to a private address: ${url.hostname}`)
    }
    const addresses =
        isIP(hostname) === 0
            ? (await lookup(hostname, { all: true })).map((result) => result.address)
            : [hostname]
    if (!options.allowPrivateNetwork && addresses.some((address) => isPrivateAddress(address)))
        throw new Error(`Import URL resolves to a private address: ${url.hostname}`)
    if (options.allowPrivateNetwork) return null
    const pinnedUrls = addresses.map((address) => {
        const host = isIP(address) === 6 ? `[${address}]` : address
        const pinned = new URL(url.toString())
        pinned.hostname = host
        return pinned
    })
    return { pinnedUrls, serverName: hostname }
}

export { tlsCheckServerIdentity }
