// FYLO mobile bridge — hosts the local-first web engine (fylo.mjs) inside a
// native WebView (WKWebView on iOS, android.webkit.WebView on Android) and
// exposes it to Swift/Kotlin over a tiny request/response RPC.
//
// The native side calls `__fyloDispatch("<base64-json>")` (base64 keeps the
// payload safe inside an evaluateJavaScript string literal); this module runs
// the op against the local client and posts a JSON result back through the
// platform's message channel. All reads and writes hit the on-device OPFS
// store — there is no network access and no backend.
//
// Drop `fylo.mjs` (the released web bundle) next to this file in the app's
// WebView assets. This must be served from a secure origin (custom scheme on
// iOS, https via WebViewAssetLoader on Android) or OPFS is unavailable.
import { createBrowserClient } from './fylo.mjs'

let db = null
const MAX_REQUEST_BASE64 = 8 * 1024 * 1024
const MAX_RESPONSE_BYTES = 6 * 1024 * 1024
const MAX_SQL_CHARS = 64 * 1024
const MAX_COLLECTION_CHARS = 128
const MAX_ID_CHARS = 128
const OPERATIONS = new Set([
    'open',
    'createCollection',
    'dropCollection',
    'inspectCollection',
    'rebuildCollection',
    'putData',
    'getDoc',
    'getMeta',
    'setMeta',
    'getLatest',
    'patchDoc',
    'delDoc',
    'restoreDoc',
    'findDocs',
    'executeSQL'
])

function own(value, key) {
    return value && typeof value === 'object' && Object.hasOwn(value, key) ? value[key] : undefined
}

function requireBoundedString(value, name, maximum) {
    if (typeof value !== 'string' || value.length === 0 || value.length > maximum) {
        throw new Error(`${name} must be a non-empty string of at most ${maximum} characters`)
    }
    return value
}

function requireObject(value, name) {
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
        throw new Error(`${name} must be an object`)
    }
    return value
}

function mobileConfig(value) {
    if (value === undefined) return {}
    const config = requireObject(value, 'config')
    const safe = Object.create(null)
    const namespace = own(config, 'namespace')
    const root = own(config, 'root')
    const worker = own(config, 'worker')
    if (namespace !== undefined) {
        safe.namespace = requireBoundedString(namespace, 'config.namespace', 128)
    }
    if (root !== undefined) {
        const path = requireBoundedString(root, 'config.root', 256)
        if (!path.startsWith('/') || path.includes('..') || path.includes('\\')) {
            throw new Error('config.root must be an absolute browser path without traversal')
        }
        safe.root = path
    }
    if (worker !== undefined) {
        if (typeof worker !== 'boolean') throw new Error('config.worker must be boolean')
        safe.worker = worker
    }
    safe.storage = 'opfs'
    return safe
}

function validateArgs(method, args) {
    if (!args || typeof args !== 'object' || Array.isArray(args)) {
        throw new Error('args must be an object')
    }
    if (method !== 'open' && method !== 'executeSQL') {
        const collection = requireBoundedString(
            own(args, 'collection'),
            'collection',
            MAX_COLLECTION_CHARS
        )
        if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(collection)) {
            throw new Error('collection contains unsupported characters')
        }
    }
    if (
        ['getDoc', 'getMeta', 'setMeta', 'getLatest', 'patchDoc', 'delDoc', 'restoreDoc'].includes(
            method
        )
    ) {
        requireBoundedString(own(args, 'id'), 'id', MAX_ID_CHARS)
    }
    if (method === 'executeSQL') {
        requireBoundedString(own(args, 'sql'), 'sql', MAX_SQL_CHARS)
    }
    if (method === 'putData') requireObject(own(args, 'data'), 'data')
    if (method === 'setMeta') requireObject(own(args, 'meta'), 'meta')
    if (method === 'patchDoc') requireObject(own(args, 'newDoc'), 'newDoc')
    if (method === 'findDocs' && own(args, 'query') !== undefined) {
        requireObject(own(args, 'query'), 'query')
    }
    return args
}

function rawPost(text) {
    if (globalThis.flutter_inappwebview?.callHandler) {
        globalThis.flutter_inappwebview.callHandler('fylo', text) // Flutter (flutter_inappwebview)
    } else if (globalThis.webkit?.messageHandlers?.fylo) {
        globalThis.webkit.messageHandlers.fylo.postMessage(text) // iOS (native Swift)
    } else if (globalThis.__fyloNative?.onMessage) {
        globalThis.__fyloNative.onMessage(text) // Android (native Kotlin)
    } else {
        return false
    }
    return true
}

function post(message) {
    let text = JSON.stringify(message)
    if (new TextEncoder().encode(text).byteLength > MAX_RESPONSE_BYTES) {
        text = JSON.stringify({
            id: own(message, 'id') ?? null,
            ok: false,
            error: { message: 'FYLO bridge response exceeds the 6 MiB limit' }
        })
    }
    if (rawPost(text)) return
    // Channel not live yet (flutter_inappwebview injects its bridge asynchronously
    // and signals `flutterInAppWebViewPlatformReady`). Retry on that event and by
    // a short poll, so the initial "ready" post is never dropped. Native Swift/
    // Kotlin register their channel before load, so they take the fast path above.
    const retry = () => rawPost(text)
    globalThis.addEventListener?.('flutterInAppWebViewPlatformReady', retry, { once: true })
    let tries = 0
    const timer = setInterval(() => {
        if (rawPost(text) || ++tries > 100) clearInterval(timer)
    }, 20)
}

async function ensureDb(config) {
    if (!db) {
        db = createBrowserClient(mobileConfig(config))
        await db.ready()
    }
    return db
}

async function collect(cursor) {
    const out = Object.create(null)
    for await (const page of cursor.collect()) {
        for (const [key, value] of Object.entries(page)) out[key] = value
    }
    return out
}

async function route(method, a) {
    if (!OPERATIONS.has(method)) throw new Error('unknown method: ' + method)
    a = validateArgs(method, a)
    const database = await ensureDb(own(a, 'config'))
    const collection = own(a, 'collection')
    const col = collection ? database[collection] : null
    switch (method) {
        case 'open':
            return { ready: true }
        case 'createCollection':
            return col.create()
        case 'dropCollection':
            return col.drop()
        case 'inspectCollection':
            return col.inspect()
        case 'rebuildCollection':
            return col.rebuild()
        case 'putData':
            return col.put(own(a, 'data'))
        case 'getDoc':
            return await col.get(own(a, 'id')).once()
        case 'getMeta':
            return col.get(own(a, 'id')).metadata()
        case 'setMeta':
            return col.put(own(a, 'id')).metadata(own(a, 'meta'))
        case 'getLatest':
            return col.latest(own(a, 'id'))
        case 'patchDoc':
            return col.patch(own(a, 'id'), own(a, 'newDoc'))
        case 'delDoc':
            return col.delete(own(a, 'id'))
        case 'restoreDoc':
            return col.restore(own(a, 'id'))
        case 'findDocs':
            return collect(col.find(own(a, 'query') ?? {}))
        case 'executeSQL':
            return database._sql(own(a, 'sql')) // raw string; local store only
        default:
            throw new Error('unknown method: ' + method)
    }
}

// UTF-8-safe base64 decode (native encodes JSON as base64 of UTF-8 bytes).
function decode(encoded) {
    if (typeof encoded !== 'string' || encoded.length > MAX_REQUEST_BASE64) {
        throw new Error('FYLO bridge request exceeds the 8 MiB limit')
    }
    if (!/^[A-Za-z0-9+/]*={0,2}$/.test(encoded)) throw new Error('Invalid base64 request')
    const binary = atob(encoded)
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0))
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes)
}

// Native clients encode `id` first. Decode only a small aligned prefix so an
// oversized or malformed request can still receive a correlated error without
// allocating the rejected body. The anchored grammar cannot mistake an id
// nested in user data for the request id.
function recoverRequestId(encoded) {
    if (typeof encoded !== 'string' || encoded.length === 0) return null
    const prefixLength = Math.min(encoded.length, 1024)
    const alignedLength = prefixLength - (prefixLength % 4)
    if (alignedLength === 0) return null
    const prefix = encoded.slice(0, alignedLength)
    if (!/^[A-Za-z0-9+/]*={0,2}$/.test(prefix)) return null
    try {
        const binary = atob(prefix)
        const text = new TextDecoder().decode(
            Uint8Array.from(binary, (character) => character.charCodeAt(0))
        )
        const match = text.match(/^\s*\{\s*"id"\s*:\s*(\d{1,15})(?=\s*[,}])/)
        const id = match ? Number(match[1]) : NaN
        return Number.isSafeInteger(id) && id > 0 ? id : null
    } catch {
        return null
    }
}

globalThis.__fyloDispatch = async (encoded) => {
    let msg
    try {
        msg = JSON.parse(decode(encoded))
    } catch (error) {
        const id = recoverRequestId(encoded)
        if (id !== null) {
            post({
                id,
                ok: false,
                error: {
                    message: String(error?.message || 'Invalid FYLO bridge request')
                }
            })
        }
        return
    }
    try {
        if (!msg || typeof msg !== 'object' || Array.isArray(msg))
            throw new Error('invalid request')
        const id = own(msg, 'id')
        if (!Number.isSafeInteger(id) || id <= 0) throw new Error('id must be a positive integer')
        const method = requireBoundedString(own(msg, 'method'), 'method', 64)
        const result = await route(method, own(msg, 'args') ?? {})
        post({ id, ok: true, result: result === undefined ? null : result })
    } catch (e) {
        post({
            id: Number.isSafeInteger(own(msg, 'id')) ? own(msg, 'id') : null,
            ok: false,
            error: { message: String((e && e.message) || e) }
        })
    }
}

// id 0 is reserved: signals the bridge is loaded and ready for requests.
post({ id: 0, ok: true, result: { bridge: 'ready' } })
