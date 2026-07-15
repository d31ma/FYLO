// FYLO iOS client — a local-first document store for Swift.
//
// Unlike the server/CLI shims, a phone can't spawn the `fylo` binary. Instead
// this hosts FYLO's local-first web engine (`fylo.mjs`) in a headless WKWebView:
// all reads and writes hit an on-device OPFS store — fully offline, no backend.
// It mirrors the browser client — same engine, native API.
//
//   let db = try await Fylo()
//   try await db.createCollection("users")
//   let id = try await db.putData("users", ["name": "Ada", "role": "admin"]) as! String
//   let doc = try await db.getLatest("users", id)
//   let admins = try await db.findDocs("users", ["$ops": [["role": ["$eq": "admin"]]]])
//
// Bundle three files as app resources: this engine's `fylo.mjs` (from a FYLO
// release), plus `host.html` and `bridge.js` from clients/mobile/. They are
// served over a custom `fylo-app://` scheme — a secure origin, which OPFS
// requires (a file:// origin does not get OPFS).
//
// Requires iOS 14+ / macOS 11+ (WebKit). The WebView is never shown.

import Foundation
import WebKit

enum FyloError: Error, CustomStringConvertible {
    case failed(String)
    var description: String {
        switch self { case .failed(let message): return message }
    }
}

@MainActor
final class Fylo: NSObject {
    private static let maxBridgeBytes = 6 * 1024 * 1024
    private static let maxPendingRequests = 256
    private static let maxRPCTimeout: TimeInterval = 5 * 60
    private struct PendingRequest {
        let continuation: CheckedContinuation<Any?, Error>
        let timeout: DispatchWorkItem
    }
    private let webView: WKWebView
    private let scheme: String
    private let rpcTimeout: TimeInterval
    private var nextId = 1
    private var pending: [Int: PendingRequest] = [:]
    private var readyContinuation: CheckedContinuation<Void, Error>?
    private var readyTimeout: DispatchWorkItem?
    private var isReady = false
    private var isClosed = false

    /// Boot the engine. Resolves once the local store is ready. `bundle` holds
    /// the three engine assets. Custom scheme host defaults to `fylo-app`.
    init(
        bundle: Bundle = .main,
        scheme: String = "fylo-app",
        rpcTimeout: TimeInterval = 30
    ) async throws {
        guard scheme.range(of: "^[a-z][a-z0-9+.-]{0,31}$", options: .regularExpression) != nil,
              !["http", "https", "file", "data", "javascript"].contains(scheme)
        else { throw FyloError.failed("invalid FYLO WebView scheme") }
        guard rpcTimeout.isFinite, rpcTimeout > 0, rpcTimeout <= Self.maxRPCTimeout else {
            throw FyloError.failed("FYLO RPC timeout must be greater than 0 and at most 300 seconds")
        }
        let config = WKWebViewConfiguration()
        let handler = AssetSchemeHandler(bundle: bundle, scheme: scheme)
        config.setURLSchemeHandler(handler, forURLScheme: scheme)
        self.scheme = scheme
        self.rpcTimeout = rpcTimeout
        webView = WKWebView(frame: .zero, configuration: config)
        super.init()
        config.userContentController.add(self, name: "fylo")
        webView.navigationDelegate = self

        var initializationSucceeded = false
        var initializationFailure: Error = FyloError.failed("FYLO WebView initialization failed")
        defer {
            if !initializationSucceeded { failBridge(initializationFailure) }
        }
        do {
            try await withTaskCancellationHandler(operation: {
                if Task.isCancelled { throw CancellationError() }
                try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
                    readyContinuation = cont
                    let timeout = DispatchWorkItem { [weak self] in
                        self?.failBridge(
                            FyloError.failed("FYLO WebView did not become ready within the configured RPC timeout")
                        )
                    }
                    readyTimeout = timeout
                    DispatchQueue.main.asyncAfter(deadline: .now() + rpcTimeout, execute: timeout)
                    webView.load(URLRequest(url: URL(string: "\(scheme)://app/host.html")!))
                }
            }, onCancel: {
                Task { @MainActor [weak self] in
                    self?.failBridge(CancellationError())
                }
            })
            // First real op initializes the local client.
            _ = try await request("open", ["config": [String: Any]()])
            initializationSucceeded = true
        } catch {
            initializationFailure = error
            throw error
        }
    }

    /// Send one RPC to the engine and await its result.
    @discardableResult
    func request(_ method: String, _ args: [String: Any] = [:]) async throws -> Any? {
        guard !isClosed else { throw FyloError.failed("FYLO client is closed") }
        guard pending.count < Self.maxPendingRequests else {
            throw FyloError.failed("too many pending FYLO requests")
        }
        let id = nextId
        nextId += 1
        let payload: [String: Any] = ["id": id, "method": method, "args": args]
        let data = try JSONSerialization.data(withJSONObject: payload)
        guard data.count <= Self.maxBridgeBytes else {
            throw FyloError.failed("FYLO request exceeds the 6 MiB native bridge limit")
        }
        let base64 = data.base64EncodedString()
        return try await withTaskCancellationHandler(operation: {
            if Task.isCancelled { throw CancellationError() }
            return try await withCheckedThrowingContinuation { cont in
                let timeout = DispatchWorkItem { [weak self] in
                    self?.failRequest(
                        id: id,
                        error: FyloError.failed(
                            "FYLO request '\(method)' timed out after the configured RPC timeout"
                        )
                    )
                }
                pending[id] = PendingRequest(continuation: cont, timeout: timeout)
                DispatchQueue.main.asyncAfter(deadline: .now() + rpcTimeout, execute: timeout)
                // base64 body is safe inside the JS string literal.
                webView.evaluateJavaScript("window.__fyloDispatch(\"\(base64)\")") { _, error in
                    if let error {
                        self.failRequest(id: id, error: error)
                    }
                }
            }
        }, onCancel: {
            Task { @MainActor [weak self] in
                self?.failRequest(
                    id: id,
                    error: CancellationError()
                )
            }
        })
    }

    // MARK: Collections
    // `kind` is accepted for cross-client parity; the on-device engine stores documents untyped.
    @discardableResult func createCollection(_ collection: String, _ kind: String = "document") async throws -> Any? {
        try await request("createCollection", ["collection": collection])
    }
    @discardableResult func dropCollection(_ collection: String) async throws -> Any? {
        try await request("dropCollection", ["collection": collection])
    }
    @discardableResult func inspectCollection(_ collection: String) async throws -> Any? {
        try await request("inspectCollection", ["collection": collection])
    }
    @discardableResult func rebuildCollection(_ collection: String) async throws -> Any? {
        try await request("rebuildCollection", ["collection": collection])
    }

    // MARK: Documents
    @discardableResult func putData(_ collection: String, _ data: [String: Any]) async throws -> Any? {
        try await request("putData", ["collection": collection, "data": data])
    }
    @discardableResult func getDoc(_ collection: String, _ id: String) async throws -> Any? {
        try await request("getDoc", ["collection": collection, "id": id])
    }
    @discardableResult func getMeta(_ collection: String, _ id: String) async throws -> Any? {
        try await request("getMeta", ["collection": collection, "id": id])
    }
    @discardableResult func setMeta(_ collection: String, _ id: String, _ meta: [String: Any]) async throws -> Any? {
        try await request("setMeta", ["collection": collection, "id": id, "meta": meta])
    }
    @discardableResult func getLatest(_ collection: String, _ id: String) async throws -> Any? {
        try await request("getLatest", ["collection": collection, "id": id])
    }
    @discardableResult func patchDoc(_ collection: String, _ id: String, _ newDoc: [String: Any]) async throws -> Any? {
        try await request("patchDoc", ["collection": collection, "id": id, "newDoc": newDoc])
    }
    @discardableResult func delDoc(_ collection: String, _ id: String) async throws -> Any? {
        try await request("delDoc", ["collection": collection, "id": id])
    }
    @discardableResult func restoreDoc(_ collection: String, _ id: String) async throws -> Any? {
        try await request("restoreDoc", ["collection": collection, "id": id])
    }

    // MARK: Query
    @discardableResult func findDocs(_ collection: String, _ query: [String: Any]) async throws -> Any? {
        try await request("findDocs", ["collection": collection, "query": query])
    }

    /// Run raw SQL against the local store. Native interpolation is verbatim —
    /// escape/validate untrusted input yourself.
    @discardableResult func sql(_ statement: String) async throws -> Any? {
        try await request("executeSQL", ["sql": statement])
    }

    /// Collection-scoped facade with short method names, so
    /// `try await db.collection("users").put(data)` reads like the browser client.
    func collection(_ name: String) -> FyloCollection {
        FyloCollection(db: self, name: name)
    }

    /// Stop the private WebView and fail every request that has not completed.
    /// Calling `close()` more than once is safe.
    func close() {
        failBridge(FyloError.failed("FYLO client is closed"))
    }

    private func failRequest(id: Int, error: Error) {
        guard let request = pending.removeValue(forKey: id) else { return }
        request.timeout.cancel()
        request.continuation.resume(throwing: error)
    }

    private func failBridge(_ error: Error) {
        guard !isClosed else { return }
        isClosed = true
        isReady = false
        readyTimeout?.cancel()
        readyTimeout = nil
        readyContinuation?.resume(throwing: error)
        readyContinuation = nil
        let requests = Array(pending.values)
        pending.removeAll()
        for request in requests {
            request.timeout.cancel()
            request.continuation.resume(throwing: error)
        }
        webView.stopLoading()
        webView.navigationDelegate = nil
        webView.configuration.userContentController.removeScriptMessageHandler(forName: "fylo")
    }
}

/// A collection-scoped view; methods drop the leading collection argument.
@MainActor
struct FyloCollection {
    let db: Fylo
    let name: String

    @discardableResult func create(_ kind: String = "document") async throws -> Any? {
        try await db.createCollection(name, kind)
    }
    @discardableResult func drop() async throws -> Any? { try await db.dropCollection(name) }
    @discardableResult func inspect() async throws -> Any? { try await db.inspectCollection(name) }
    @discardableResult func rebuild() async throws -> Any? { try await db.rebuildCollection(name) }
    @discardableResult func put(_ data: [String: Any]) async throws -> Any? {
        try await db.putData(name, data)
    }
    @discardableResult func get(_ id: String) async throws -> Any? { try await db.getDoc(name, id) }
    @discardableResult func getMeta(_ id: String) async throws -> Any? { try await db.getMeta(name, id) }
    @discardableResult func setMeta(_ id: String, _ meta: [String: Any]) async throws -> Any? {
        try await db.setMeta(name, id, meta)
    }
    @discardableResult func latest(_ id: String) async throws -> Any? {
        try await db.getLatest(name, id)
    }
    @discardableResult func patch(_ id: String, _ newDoc: [String: Any]) async throws -> Any? {
        try await db.patchDoc(name, id, newDoc)
    }
    @discardableResult func delete(_ id: String) async throws -> Any? { try await db.delDoc(name, id) }
    @discardableResult func restore(_ id: String) async throws -> Any? {
        try await db.restoreDoc(name, id)
    }
    @discardableResult func find(_ query: [String: Any]) async throws -> Any? {
        try await db.findDocs(name, query)
    }
}

// MARK: - WKScriptMessageHandler (JS → native replies)
extension Fylo: WKScriptMessageHandler {
    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let text = message.body as? String else {
            if let reply = message.body as? [String: Any], let id = reply["id"] as? Int {
                failCorrelated(id: id, message: "FYLO bridge reply must be a UTF-8 JSON string")
            }
            return
        }
        let recoveredId = Self.recoverId(from: text)
        guard text.utf8.count <= Self.maxBridgeBytes else {
            if let id = recoveredId {
                failCorrelated(id: id, message: "FYLO bridge response exceeds the 6 MiB limit")
            }
            return
        }
        guard let data = text.data(using: .utf8),
              let reply = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = reply["id"] as? Int
        else {
            if let id = recoveredId {
                failCorrelated(id: id, message: "invalid FYLO bridge response")
            }
            return
        }
        deliver(id: id, reply: reply)
    }

    private static func recoverId(from text: String) -> Int? {
        let prefix = String(text.prefix(1024))
        let pattern = #"^\s*\{\s*"id"\s*:\s*([0-9]{1,15})(?=\s*[,}])"#
        guard let expression = try? NSRegularExpression(pattern: pattern),
              let match = expression.firstMatch(
                  in: prefix,
                  range: NSRange(prefix.startIndex..., in: prefix)
              ),
              let range = Range(match.range(at: 1), in: prefix),
              let id = Int(prefix[range]),
              id >= 0
        else { return nil }
        return id
    }

    private func failCorrelated(id: Int, message: String) {
        let error = FyloError.failed(message)
        if id == 0 {
            readyContinuation?.resume(throwing: error)
            readyContinuation = nil
            return
        }
        failRequest(id: id, error: error)
    }

    private func deliver(id: Int, reply: [String: Any]) {
        if id == 0 { // bridge-ready signal
            guard !isClosed, !isReady else { return }
            isReady = true
            readyTimeout?.cancel()
            readyTimeout = nil
            readyContinuation?.resume()
            readyContinuation = nil
            return
        }
        guard let request = pending.removeValue(forKey: id) else { return }
        request.timeout.cancel()
        if reply["ok"] as? Bool == true {
            request.continuation.resume(returning: reply["result"] ?? NSNull())
        } else {
            let error = reply["error"] as? [String: Any]
            request.continuation.resume(
                throwing: FyloError.failed(error?["message"] as? String ?? "fylo error")
            )
        }
    }
}

// Block top-level navigation away from the one bundled host document. Engine
// module subresources are served by the scheme handler and never need a page
// navigation decision.
extension Fylo: WKNavigationDelegate {
    func webView(
        _ webView: WKWebView,
        decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void
    ) {
        guard !isClosed else {
            decisionHandler(.cancel)
            return
        }
        guard let url = navigationAction.request.url,
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              url.scheme == scheme,
              url.host == "app",
              components.percentEncodedPath == "/host.html",
              url.user == nil,
              url.password == nil,
              url.port == nil,
              url.query == nil,
              url.fragment == nil
        else {
            decisionHandler(.cancel)
            failBridge(FyloError.failed("FYLO WebView blocked an unexpected navigation"))
            return
        }
        if isReady {
            decisionHandler(.cancel)
            failBridge(FyloError.failed("FYLO WebView unexpectedly navigated after becoming ready"))
            return
        }
        decisionHandler(.allow)
    }

    func webView(
        _ webView: WKWebView,
        didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        failBridge(FyloError.failed("FYLO WebView failed to load: \(error.localizedDescription)"))
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        failBridge(FyloError.failed("FYLO WebView navigation failed: \(error.localizedDescription)"))
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        failBridge(FyloError.failed("FYLO WebView content process terminated; reopen the client"))
    }
}

// MARK: - Custom-scheme asset handler (secure origin so OPFS is available)
private final class AssetSchemeHandler: NSObject, WKURLSchemeHandler {
    private static let assets: [String: (base: String, ext: String, mime: String)] = [
        "/host.html": ("host", "html", "text/html"),
        "/bridge.js": ("bridge", "js", "text/javascript"),
        "/fylo.mjs": ("fylo", "mjs", "text/javascript")
    ]
    private let bundle: Bundle
    private let scheme: String
    init(bundle: Bundle, scheme: String) {
        self.bundle = bundle
        self.scheme = scheme
    }

    func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        guard let url = task.request.url else {
            task.didFailWithError(FyloError.failed("no url"))
            return
        }
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              url.scheme == scheme,
              url.host == "app",
              url.user == nil,
              url.password == nil,
              url.port == nil,
              url.query == nil,
              url.fragment == nil,
              let asset = Self.assets[components.percentEncodedPath],
              let fileURL = bundle.url(forResource: asset.base, withExtension: asset.ext),
              let data = try? Data(contentsOf: fileURL) else {
            task.didFailWithError(FyloError.failed("FYLO WebView asset is not allowed"))
            return
        }
        var headers = [
            "Content-Type": "\(asset.mime); charset=utf-8",
            "X-Content-Type-Options": "nosniff",
            "Cache-Control": "no-store"
        ]
        if asset.ext == "html" {
            headers["Content-Security-Policy"] = "default-src 'none'; script-src 'self'; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        }
        let response = HTTPURLResponse(
            url: url,
            statusCode: 200,
            httpVersion: "HTTP/1.1",
            headerFields: headers
        )!
        task.didReceive(response)
        task.didReceive(data)
        task.didFinish()
    }

    func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {}
}
