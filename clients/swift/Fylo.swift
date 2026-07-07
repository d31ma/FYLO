// FYLO iOS client — a local-first document store for Swift.
//
// Unlike the server/CLI shims, a phone can't spawn the `fylo` binary. Instead
// this hosts FYLO's local-first web engine (`fylo.mjs`) in a headless WKWebView:
// reads and writes hit an on-device OPFS store (fully offline), and a background
// SyncEngine reconciles with a backend `fylo serve` over REST/SSE. It mirrors
// the browser client — same engine, native API.
//
//   let db = try await Fylo(serverUrl: "https://api.example.com", token: token)
//   try await db.createCollection("users")
//   let id = try await db.putData("users", ["name": "Ada", "role": "admin"]) as! String
//   let doc = try await db.getLatest("users", id)
//   let admins = try await db.findDocs("users", ["$ops": [["role": ["$eq": "admin"]]]])
//   await db.syncStart() // begin background sync; omit serverUrl for offline-only
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
    private let webView: WKWebView
    private var nextId = 1
    private var pending: [Int: CheckedContinuation<Any?, Error>] = [:]
    private var readyContinuation: CheckedContinuation<Void, Error>?
    private var isReady = false

    /// Boot the engine. Resolves once the local store is ready; `serverUrl`/`token`
    /// enable backend sync (omit for a pure offline store). `bundle` holds the
    /// three engine assets. Custom scheme host defaults to `fylo-app`.
    init(serverUrl: String? = nil, token: String? = nil, bundle: Bundle = .main, scheme: String = "fylo-app") async throws {
        let config = WKWebViewConfiguration()
        let handler = AssetSchemeHandler(bundle: bundle)
        config.setURLSchemeHandler(handler, forURLScheme: scheme)
        webView = WKWebView(frame: .zero, configuration: config)
        super.init()
        config.userContentController.add(self, name: "fylo")

        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            readyContinuation = cont
            webView.load(URLRequest(url: URL(string: "\(scheme)://app/host.html")!))
        }
        // First real op initializes the synced client with the sync config.
        var cfg: [String: Any] = [:]
        if let serverUrl { cfg["serverUrl"] = serverUrl }
        if let token { cfg["token"] = token }
        _ = try await request("open", ["config": cfg])
    }

    /// Send one RPC to the engine and await its result.
    @discardableResult
    func request(_ method: String, _ args: [String: Any] = [:]) async throws -> Any? {
        let id = nextId
        nextId += 1
        let payload: [String: Any] = ["id": id, "method": method, "args": args]
        let data = try JSONSerialization.data(withJSONObject: payload)
        let base64 = data.base64EncodedString()
        return try await withCheckedThrowingContinuation { cont in
            pending[id] = cont
            // base64 body is safe inside the JS string literal.
            webView.evaluateJavaScript("window.__fyloDispatch(\"\(base64)\")") { _, error in
                if let error, self.pending[id] != nil {
                    self.pending.removeValue(forKey: id)?.resume(throwing: error)
                }
            }
        }
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
    /// escape/validate untrusted input yourself. SQL writes are local-only (not
    /// pushed); use the document methods above to sync writes.
    @discardableResult func sql(_ statement: String) async throws -> Any? {
        try await request("executeSQL", ["sql": statement])
    }

    // MARK: Sync
    func syncStart() async throws { _ = try await request("syncStart") }
    func syncStop() async throws { _ = try await request("syncStop") }
    func isOnline() async throws -> Bool {
        let result = try await request("online") as? [String: Any]
        return (result?["online"] as? Bool) ?? false
    }

    /// Collection-scoped facade with short method names, so
    /// `try await db.collection("users").put(data)` reads like the browser client.
    func collection(_ name: String) -> FyloCollection {
        FyloCollection(db: self, name: name)
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
        guard let text = message.body as? String,
              let data = text.data(using: .utf8),
              let reply = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = reply["id"] as? Int
        else { return }
        deliver(id: id, reply: reply)
    }

    private func deliver(id: Int, reply: [String: Any]) {
        if id == 0 { // bridge-ready signal
            isReady = true
            readyContinuation?.resume()
            readyContinuation = nil
            return
        }
        guard let cont = pending.removeValue(forKey: id) else { return }
        if reply["ok"] as? Bool == true {
            cont.resume(returning: reply["result"] ?? NSNull())
        } else {
            let error = reply["error"] as? [String: Any]
            cont.resume(throwing: FyloError.failed(error?["message"] as? String ?? "fylo error"))
        }
    }
}

// MARK: - Custom-scheme asset handler (secure origin so OPFS is available)
private final class AssetSchemeHandler: NSObject, WKURLSchemeHandler {
    private let bundle: Bundle
    init(bundle: Bundle) { self.bundle = bundle }

    func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        guard let url = task.request.url else {
            task.didFailWithError(FyloError.failed("no url"))
            return
        }
        // Map "<scheme>://app/<name.ext>" to the bundled resource of that name.
        let name = url.lastPathComponent.isEmpty ? "host.html" : url.lastPathComponent
        let ext = (name as NSString).pathExtension
        let base = (name as NSString).deletingPathExtension
        guard let fileURL = bundle.url(forResource: base, withExtension: ext),
              let data = try? Data(contentsOf: fileURL) else {
            task.didFailWithError(FyloError.failed("missing asset: \(name)"))
            return
        }
        let mime: String
        switch ext {
        case "html": mime = "text/html"
        case "js", "mjs": mime = "text/javascript"
        case "json": mime = "application/json"
        default: mime = "application/octet-stream"
        }
        let response = HTTPURLResponse(
            url: url,
            statusCode: 200,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "\(mime); charset=utf-8"]
        )!
        task.didReceive(response)
        task.didReceive(data)
        task.didFinish()
    }

    func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {}
}
