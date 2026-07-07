// FYLO Android client — a local-first document store for Kotlin.
//
// A phone can't spawn the `fylo` binary, so this hosts FYLO's local-first web
// engine (`fylo.mjs`) in a headless android.webkit.WebView: reads and writes hit
// an on-device OPFS store (fully offline), and a background SyncEngine reconciles
// with a backend `fylo serve` over REST/SSE. It mirrors the browser client —
// same engine, native API.
//
//   val db = Fylo.open(context, serverUrl = "https://api.example.com", token = token)
//   db.createCollection("users")
//   val id = db.putData("users", mapOf("name" to "Ada", "role" to "admin")) as String
//   val doc = db.getLatest("users", id)
//   val admins = db.findDocs("users", mapOf("\$ops" to listOf(mapOf("role" to mapOf("\$eq" to "admin")))))
//   db.syncStart() // begin background sync; omit serverUrl for offline-only
//
// Bundle three files under the app's assets at `assets/fylo/`: this engine's
// `fylo.mjs` (from a FYLO release), plus `host.html` and `bridge.js` from
// clients/mobile/. They are served over an https origin (a secure context,
// which OPFS requires — a file:// origin does not get OPFS) via request
// interception; no network is used for the assets.
//
// Platform only: android.webkit + org.json (both in the SDK) and
// kotlinx.coroutines (standard on Android). No third-party HTTP/JSON libs.
// Requires a Chromium WebView with OPFS (Android System WebView / Chrome 108+).

import android.annotation.SuppressLint
import android.content.Context
import android.webkit.JavascriptInterface
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.io.ByteArrayInputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger

class Fylo private constructor(
    private val webView: WebView,
) {
    private val nextId = AtomicInteger(1)
    private val pending = ConcurrentHashMap<Int, CompletableDeferred<Any?>>()
    private val ready = CompletableDeferred<Unit>()

    /** Send one RPC to the engine and await its result (call off the main thread). */
    suspend fun request(method: String, args: Map<String, Any?> = emptyMap()): Any? {
        val id = nextId.getAndIncrement()
        val payload = JSONObject()
            .put("id", id)
            .put("method", method)
            .put("args", toJsonObject(args))
        val base64 = android.util.Base64.encodeToString(
            payload.toString().toByteArray(Charsets.UTF_8),
            android.util.Base64.NO_WRAP,
        )
        val deferred = CompletableDeferred<Any?>()
        pending[id] = deferred
        withContext(Dispatchers.Main) {
            // base64 body is safe inside the JS string literal.
            webView.evaluateJavascript("window.__fyloDispatch(\"$base64\")", null)
        }
        return deferred.await()
    }

    // JS -> native bridge target (runs on a WebView binder thread).
    private inner class Bridge {
        @JavascriptInterface
        fun onMessage(text: String) {
            val reply = JSONObject(text)
            val id = reply.optInt("id", -1)
            if (id == 0) { // bridge-ready signal
                ready.complete(Unit)
                return
            }
            val deferred = pending.remove(id) ?: return
            if (reply.optBoolean("ok", false)) {
                deferred.complete(if (reply.isNull("result")) null else reply.opt("result"))
            } else {
                val message = reply.optJSONObject("error")?.optString("message") ?: "fylo error"
                deferred.completeExceptionally(FyloException(message))
            }
        }
    }

    // --- Collections ---
    // `kind` is accepted for cross-client parity; the on-device engine stores documents untyped.
    suspend fun createCollection(collection: String, kind: String = "document"): Any? =
        request("createCollection", mapOf("collection" to collection))
    suspend fun dropCollection(collection: String): Any? =
        request("dropCollection", mapOf("collection" to collection))
    suspend fun inspectCollection(collection: String): Any? =
        request("inspectCollection", mapOf("collection" to collection))
    suspend fun rebuildCollection(collection: String): Any? =
        request("rebuildCollection", mapOf("collection" to collection))

    // --- Documents (object args are native Maps/Lists) ---
    suspend fun putData(collection: String, data: Map<String, Any?>): Any? =
        request("putData", mapOf("collection" to collection, "data" to data))
    suspend fun getDoc(collection: String, id: String): Any? =
        request("getDoc", mapOf("collection" to collection, "id" to id))
    suspend fun getLatest(collection: String, id: String): Any? =
        request("getLatest", mapOf("collection" to collection, "id" to id))
    suspend fun patchDoc(collection: String, id: String, newDoc: Map<String, Any?>): Any? =
        request("patchDoc", mapOf("collection" to collection, "id" to id, "newDoc" to newDoc))
    suspend fun delDoc(collection: String, id: String): Any? =
        request("delDoc", mapOf("collection" to collection, "id" to id))
    suspend fun restoreDoc(collection: String, id: String): Any? =
        request("restoreDoc", mapOf("collection" to collection, "id" to id))

    // --- Query ---
    suspend fun findDocs(collection: String, query: Map<String, Any?>): Any? =
        request("findDocs", mapOf("collection" to collection, "query" to query))

    /**
     * Run raw SQL against the local store. Native interpolation is verbatim —
     * escape/validate untrusted input yourself. SQL writes are local-only (not
     * pushed); use the document methods above to sync writes.
     */
    suspend fun sql(statement: String): Any? =
        request("executeSQL", mapOf("sql" to statement))

    // --- Sync ---
    suspend fun syncStart() { request("syncStart") }
    suspend fun syncStop() { request("syncStop") }
    suspend fun isOnline(): Boolean =
        ((request("online") as? JSONObject)?.optBoolean("online", false)) ?: false

    /**
     * Collection-scoped facade with short method names, so
     * `db.collection("users").put(data)` reads like the browser client.
     */
    fun collection(name: String): Collection = Collection(name)

    /** A collection-scoped view; methods drop the leading collection argument. */
    inner class Collection(private val name: String) {
        suspend fun create(kind: String = "document") = createCollection(name, kind)
        suspend fun drop() = dropCollection(name)
        suspend fun inspect() = inspectCollection(name)
        suspend fun rebuild() = rebuildCollection(name)
        suspend fun put(data: Map<String, Any?>) = putData(name, data)
        suspend fun get(id: String) = getDoc(name, id)
        suspend fun latest(id: String) = getLatest(name, id)
        suspend fun patch(id: String, newDoc: Map<String, Any?>) = patchDoc(name, id, newDoc)
        suspend fun delete(id: String) = delDoc(name, id)
        suspend fun restore(id: String) = restoreDoc(name, id)
        suspend fun find(query: Map<String, Any?>) = findDocs(name, query)
    }

    /** Release the WebView. Call from the main thread. */
    fun close() {
        webView.removeJavascriptInterface("__fyloNative")
        webView.destroy()
    }

    class FyloException(message: String) : RuntimeException(message)

    companion object {
        private const val HOST = "fylo.localhost"

        /**
         * Boot the engine. Suspends until the local store is ready; `serverUrl`/
         * `token` enable backend sync (omit for a pure offline store).
         */
        @SuppressLint("SetJavaScriptEnabled")
        suspend fun open(context: Context, serverUrl: String? = null, token: String? = null): Fylo {
            val client = withContext(Dispatchers.Main) {
                val webView = WebView(context)
                webView.settings.javaScriptEnabled = true
                webView.settings.domStorageEnabled = true
                val instance = Fylo(webView)
                webView.addJavascriptInterface(instance.Bridge(), "__fyloNative")
                // Serve the bundled assets over an https origin so OPFS is granted;
                // intercept every request to that origin — no network is used.
                webView.webViewClient = object : WebViewClient() {
                    override fun shouldInterceptRequest(
                        view: WebView,
                        req: WebResourceRequest,
                    ): WebResourceResponse? {
                        if (req.url.host != HOST) return null
                        val name = req.url.lastPathSegment ?: "host.html"
                        return try {
                            val bytes = context.assets.open("fylo/$name").readBytes()
                            WebResourceResponse(mimeFor(name), "utf-8", ByteArrayInputStream(bytes))
                        } catch (_: Exception) {
                            WebResourceResponse("text/plain", "utf-8", 404, "Not Found", null, null)
                        }
                    }
                }
                webView.loadUrl("https://$HOST/host.html")
                instance
            }
            client.ready.await() // resolves when the bridge posts id 0
            val config = JSONObject()
            if (serverUrl != null) config.put("serverUrl", serverUrl)
            if (token != null) config.put("token", token)
            client.request("open", mapOf("config" to config))
            return client
        }

        // Recursively convert native containers to org.json types so nested
        // objects/arrays serialize correctly (JSONObject(Map) does not deep-wrap).
        private fun toJsonObject(map: Map<*, *>): JSONObject {
            val obj = JSONObject()
            for ((key, value) in map) obj.put(key.toString(), toJsonValue(value))
            return obj
        }

        private fun toJsonValue(value: Any?): Any {
            return when (value) {
                null -> JSONObject.NULL
                is Map<*, *> -> toJsonObject(value)
                is Iterable<*> -> JSONArray().apply { value.forEach { put(toJsonValue(it)) } }
                is Array<*> -> JSONArray().apply { value.forEach { put(toJsonValue(it)) } }
                else -> value
            }
        }

        private fun mimeFor(name: String): String = when {
            name.endsWith(".html") -> "text/html"
            name.endsWith(".js") || name.endsWith(".mjs") -> "text/javascript"
            name.endsWith(".json") -> "application/json"
            else -> "application/octet-stream"
        }
    }
}
