// FYLO Android client — a local-first document store for Kotlin.
//
// A phone can't spawn the `fylo` binary, so this hosts FYLO's local-first web
// engine (`fylo.mjs`) in a headless android.webkit.WebView: all reads and
// writes hit an on-device OPFS store — fully offline, no backend. It mirrors
// the browser client — same engine, native API.
//
//   val db = Fylo.open(context)
//   db.createCollection("users")
//   val id = db.putData("users", mapOf("name" to "Ada", "role" to "admin")) as String
//   val doc = db.getLatest("users", id)
//   val admins = db.findDocs("users", mapOf("\$ops" to listOf(mapOf("role" to mapOf("\$eq" to "admin")))))
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
import android.graphics.Bitmap
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.webkit.JavascriptInterface
import android.webkit.RenderProcessGoneDetail
import android.webkit.WebResourceRequest
import android.webkit.WebResourceError
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.json.JSONArray
import org.json.JSONObject
import java.io.ByteArrayInputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

class Fylo private constructor(
    private val webView: WebView,
    private val rpcTimeoutMillis: Long,
) {
    private val nextId = AtomicInteger(1)
    private val pending = ConcurrentHashMap<Int, CompletableDeferred<Any?>>()
    private val ready = CompletableDeferred<Unit>()
    private val closed = AtomicBoolean(false)
    private val disposed = AtomicBoolean(false)

    /** Send one RPC to the engine and await its result (call off the main thread). */
    suspend fun request(method: String, args: Map<String, Any?> = emptyMap()): Any? {
        if (closed.get()) throw FyloException("FYLO client is closed; reopen it before retrying")
        val id = nextId.getAndIncrement()
        val payload = JSONObject()
            .put("id", id)
            .put("method", method)
            .put("args", toJsonObject(args))
        val payloadBytes = payload.toString().toByteArray(Charsets.UTF_8)
        if (payloadBytes.size > MAX_BRIDGE_REQUEST_BYTES) {
            throw FyloException("FYLO request exceeds the 6 MiB native bridge limit")
        }
        val base64 = android.util.Base64.encodeToString(
            payloadBytes,
            android.util.Base64.NO_WRAP,
        )
        val deferred = CompletableDeferred<Any?>()
        synchronized(pending) {
            if (closed.get()) {
                throw FyloException("FYLO client is closed; reopen it before retrying")
            }
            if (pending.size >= MAX_PENDING_REQUESTS) {
                throw FyloException("too many pending FYLO requests")
            }
            pending[id] = deferred
        }
        try {
            val completed = withTimeoutOrNull(rpcTimeoutMillis) {
                withContext(Dispatchers.Main) {
                    // base64 body is safe inside the JS string literal.
                    webView.evaluateJavascript("window.__fyloDispatch(\"$base64\")", null)
                }
                listOf(deferred.await())
            }
            if (completed != null) return completed.single()
            val error = FyloException(
                "FYLO request '$method' timed out after ${rpcTimeoutMillis}ms; verify the WebView is responsive",
            )
            if (removePending(id, deferred)) deferred.completeExceptionally(error)
            throw error
        } catch (error: CancellationException) {
            if (removePending(id, deferred)) deferred.cancel(error)
            throw error
        } finally {
            if (removePending(id, deferred) && !deferred.isCompleted) {
                deferred.cancel(CancellationException("FYLO request was cancelled"))
            }
        }
    }

    // JS -> native bridge target (runs on a WebView binder thread).
    private inner class Bridge {
        @JavascriptInterface
        fun onMessage(text: String) {
            val recoveredId = recoverReplyId(text)
            if (text.toByteArray(Charsets.UTF_8).size > MAX_BRIDGE_RESPONSE_BYTES) {
                recoveredId?.let {
                    failCorrelated(it, "FYLO bridge response exceeds the 6 MiB limit")
                }
                return
            }
            val reply = try {
                JSONObject(text)
            } catch (_: Exception) {
                recoveredId?.let { failCorrelated(it, "invalid FYLO bridge response") }
                return
            }
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

        private fun failCorrelated(id: Int, message: String) {
            val error = FyloException(message)
            if (id == 0) {
                ready.completeExceptionally(error)
                return
            }
            pending.remove(id)?.completeExceptionally(error)
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
    suspend fun getMeta(collection: String, id: String): Any? =
        request("getMeta", mapOf("collection" to collection, "id" to id))
    suspend fun setMeta(collection: String, id: String, meta: Map<String, Any?>): Any? =
        request("setMeta", mapOf("collection" to collection, "id" to id, "meta" to meta))
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
     * escape/validate untrusted input yourself.
     */
    suspend fun sql(statement: String): Any? =
        request("executeSQL", mapOf("sql" to statement))

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
        suspend fun getMeta(id: String) = getMeta(name, id)
        suspend fun setMeta(id: String, meta: Map<String, Any?>) = setMeta(name, id, meta)
        suspend fun latest(id: String) = getLatest(name, id)
        suspend fun patch(id: String, newDoc: Map<String, Any?>) = patchDoc(name, id, newDoc)
        suspend fun delete(id: String) = delDoc(name, id)
        suspend fun restore(id: String) = restoreDoc(name, id)
        suspend fun find(query: Map<String, Any?>) = findDocs(name, query)
    }

    /** Release the WebView. Call from the main thread. */
    fun close() {
        failBridge(FyloException("FYLO client is closed; reopen it before retrying"))
        disposeWebView()
    }

    private fun failBridge(error: FyloException) {
        if (!closed.compareAndSet(false, true)) return
        ready.completeExceptionally(error)
        val requests = synchronized(pending) {
            val snapshot = pending.values.toList()
            pending.clear()
            snapshot
        }
        requests.forEach { it.completeExceptionally(error) }
    }

    private fun removePending(id: Int, expected: CompletableDeferred<Any?>): Boolean =
        synchronized(pending) {
            if (pending[id] !== expected) return@synchronized false
            pending.remove(id)
            true
        }

    private fun disposeWebView() {
        if (!disposed.compareAndSet(false, true)) return
        val dispose = Runnable {
            webView.stopLoading()
            webView.removeJavascriptInterface("__fyloNative")
            webView.destroy()
        }
        if (Looper.myLooper() == Looper.getMainLooper()) dispose.run()
        else Handler(Looper.getMainLooper()).post(dispose)
    }

    class FyloException(message: String) : RuntimeException(message)

    companion object {
        private const val HOST = "fylo.localhost"
        private const val MAX_BRIDGE_REQUEST_BYTES = 6 * 1024 * 1024
        private const val MAX_BRIDGE_RESPONSE_BYTES = 6 * 1024 * 1024
        private const val MAX_PENDING_REQUESTS = 256
        private const val DEFAULT_RPC_TIMEOUT_MILLIS = 30_000L
        private const val MAX_RPC_TIMEOUT_MILLIS = 5 * 60_000L
        private val ALLOWED_ASSET_PATHS = setOf("/host.html", "/bridge.js", "/fylo.mjs")

        /** Boot the engine. Suspends until the local store is ready. */
        @SuppressLint("SetJavaScriptEnabled")
        suspend fun open(
            context: Context,
            rpcTimeoutMillis: Long = DEFAULT_RPC_TIMEOUT_MILLIS,
        ): Fylo {
            require(rpcTimeoutMillis in 1..MAX_RPC_TIMEOUT_MILLIS) {
                "FYLO RPC timeout must be between 1ms and 300000ms"
            }
            var candidate: Fylo? = null
            var initializationSucceeded = false
            var initializationFailure = FyloException("FYLO WebView initialization failed")
            try {
                val client = withTimeoutOrNull(rpcTimeoutMillis) {
                    val instance = withContext(Dispatchers.Main) {
                    val created = Fylo(WebView(context), rpcTimeoutMillis)
                    candidate = created
                    created.webView.settings.javaScriptEnabled = true
                    created.webView.settings.domStorageEnabled = true
                    created.webView.addJavascriptInterface(created.Bridge(), "__fyloNative")
                    // Serve the bundled assets over an https origin so OPFS is granted;
                    // intercept every request to that origin — no network is used.
                    created.webView.webViewClient = object : WebViewClient() {
                    override fun shouldOverrideUrlLoading(view: WebView, req: WebResourceRequest): Boolean {
                        val allowed = isAllowedUrl(req.url, documentOnly = true)
                        if (!allowed) {
                            created.failBridge(
                                FyloException("FYLO WebView blocked an unexpected navigation"),
                            )
                            created.disposeWebView()
                        } else if (created.ready.isCompleted) {
                            created.failBridge(
                                FyloException("FYLO WebView unexpectedly navigated after becoming ready"),
                            )
                            created.disposeWebView()
                            return true
                        }
                        return !allowed
                    }

                    override fun onPageStarted(view: WebView, url: String, favicon: Bitmap?) {
                        if (created.ready.isCompleted && !created.closed.get()) {
                            created.failBridge(
                                FyloException("FYLO WebView unexpectedly reloaded after becoming ready"),
                            )
                            created.disposeWebView()
                        }
                    }

                    override fun onReceivedError(
                        view: WebView,
                        req: WebResourceRequest,
                        error: WebResourceError,
                    ) {
                        if (req.isForMainFrame || isAllowedUrl(req.url)) {
                            created.failBridge(
                                FyloException("FYLO WebView failed to load: ${error.description}"),
                            )
                            created.disposeWebView()
                        }
                    }

                    override fun onReceivedHttpError(
                        view: WebView,
                        req: WebResourceRequest,
                        response: WebResourceResponse,
                    ) {
                        if (req.isForMainFrame || isAllowedUrl(req.url)) {
                            created.failBridge(
                                FyloException("FYLO WebView load failed with HTTP ${response.statusCode}"),
                            )
                            created.disposeWebView()
                        }
                    }

                    override fun onRenderProcessGone(
                        view: WebView,
                        detail: RenderProcessGoneDetail,
                    ): Boolean {
                        created.failBridge(
                            FyloException("FYLO WebView render process terminated; reopen the client"),
                        )
                        created.disposeWebView()
                        return true
                    }

                    override fun shouldInterceptRequest(
                        view: WebView,
                        req: WebResourceRequest,
                    ): WebResourceResponse {
                        if (req.method != "GET" || !isAllowedUrl(req.url)) return deniedResponse()
                        val name = req.url.lastPathSegment!!
                        return try {
                            val bytes = context.assets.open("fylo/$name").readBytes()
                            WebResourceResponse(mimeFor(name), "utf-8", ByteArrayInputStream(bytes))
                        } catch (_: Exception) {
                            WebResourceResponse("text/plain", "utf-8", 404, "Not Found", null, null)
                        }
                        }
                    }
                    created.webView.loadUrl("https://$HOST/host.html")
                    created
                }
                    instance.ready.await()
                    instance
                }
                if (client == null) {
                    throw FyloException("FYLO WebView did not become ready within ${rpcTimeoutMillis}ms")
                }
                client.request("open", mapOf("config" to JSONObject()))
                initializationSucceeded = true
                return client
            } catch (error: CancellationException) {
                initializationFailure =
                    FyloException("FYLO WebView initialization was cancelled; reopen the client")
                throw error // Preserve structured coroutine cancellation.
            } catch (error: Throwable) {
                initializationFailure = FyloException(
                    "FYLO WebView initialization failed: ${error.message ?: error::class.java.simpleName}",
                )
                throw error
            } finally {
                if (!initializationSucceeded) {
                    candidate?.failBridge(initializationFailure)
                    candidate?.disposeWebView()
                }
            }
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

        private fun isAllowedUrl(url: Uri, documentOnly: Boolean = false): Boolean {
            val path = url.encodedPath ?: return false
            return url.scheme == "https" &&
                url.host == HOST &&
                url.userInfo == null &&
                url.port == -1 &&
                url.query == null &&
                url.fragment == null &&
                path in ALLOWED_ASSET_PATHS &&
                (!documentOnly || path == "/host.html")
        }

        private fun deniedResponse(): WebResourceResponse =
            WebResourceResponse("text/plain", "utf-8", 403, "Forbidden", emptyMap(), null)

        private fun recoverReplyId(text: String): Int? {
            val match = Regex("""^\s*\{\s*"id"\s*:\s*([0-9]{1,15})(?=\s*[,}])""")
                .find(text.take(1024))
            return match?.groupValues?.get(1)?.toIntOrNull()?.takeIf { it >= 0 }
        }

        private fun mimeFor(name: String): String = when {
            name.endsWith(".html") -> "text/html"
            name.endsWith(".js") || name.endsWith(".mjs") -> "text/javascript"
            name.endsWith(".json") -> "application/json"
            else -> "application/octet-stream"
        }
    }
}
