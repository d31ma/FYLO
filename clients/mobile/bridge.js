// FYLO mobile bridge — hosts the local-first web engine (fylo.mjs) inside a
// native WebView (WKWebView on iOS, android.webkit.WebView on Android) and
// exposes it to Swift/Kotlin over a tiny request/response RPC.
//
// The native side calls `__fyloDispatch("<base64-json>")` (base64 keeps the
// payload safe inside an evaluateJavaScript string literal); this module runs
// the op against the synced client and posts a JSON result back through the
// platform's message channel. Reads hit the local OPFS store (works offline);
// writes apply locally and the SyncEngine reconciles with the backend.
//
// Drop `fylo.mjs` (the released web bundle) next to this file in the app's
// WebView assets. This must be served from a secure origin (custom scheme on
// iOS, https via WebViewAssetLoader on Android) or OPFS is unavailable.
import { createSyncedClient } from './fylo.mjs'

let db = null

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
    const text = JSON.stringify(message)
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
        db = createSyncedClient(config || {})
        await db.ready()
    }
    return db
}

async function collect(cursor) {
    const out = {}
    for await (const page of cursor.collect()) Object.assign(out, page)
    return out
}

async function route(method, a) {
    const database = await ensureDb(a.config)
    const col = a.collection ? database[a.collection] : null
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
            return col.put(a.data)
        case 'getDoc':
            return col.get(a.id)
        case 'getLatest':
            return col.latest(a.id)
        case 'patchDoc':
            return col.patch(a.id, a.newDoc)
        case 'delDoc':
            return col.delete(a.id)
        case 'restoreDoc':
            return col.restore(a.id)
        case 'findDocs':
            return collect(col.find(a.query))
        case 'executeSQL':
            return database._sql(a.sql) // raw string; local store only
        case 'syncStart':
            return database.sync.start()
        case 'syncStop':
            return database.sync.stop()
        case 'online':
            return { online: database.sync.online }
        default:
            throw new Error('unknown method: ' + method)
    }
}

// UTF-8-safe base64 decode (native encodes JSON as base64 of UTF-8 bytes).
function decode(encoded) {
    return decodeURIComponent(escape(atob(encoded)))
}

globalThis.__fyloDispatch = async (encoded) => {
    let msg
    try {
        msg = JSON.parse(decode(encoded))
    } catch (_) {
        return // unparseable payload — nothing to reply to
    }
    try {
        const result = await route(msg.method, msg.args || {})
        post({ id: msg.id, ok: true, result: result === undefined ? null : result })
    } catch (e) {
        post({ id: msg.id, ok: false, error: { message: String((e && e.message) || e) } })
    }
}

// id 0 is reserved: signals the bridge is loaded and ready for requests.
post({ id: 0, ok: true, result: { bridge: 'ready' } })
