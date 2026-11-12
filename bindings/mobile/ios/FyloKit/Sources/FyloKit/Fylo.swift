import CFylo
import Foundation

/// An open FYLO root or `.vdbx` database (ADR 0026).
///
/// Every call blocks and may flush to disk: call FYLO from a background
/// task, never the main thread. One `Fylo` may be used from many threads.
/// Results are the machine protocol's JSON as dictionaries.
///
///     let db = try Fylo.open(path: url.path, options: #"{"create": {"mode": "plain"}}"#)
///     defer { try? db.close() }
///     _ = try db.createCollection("notes")
///     let id = try db.put("notes", json: #"{"title": "Hello"}"#)
public final class Fylo: @unchecked Sendable {
    private let root: UInt64

    private init(root: UInt64) {
        self.root = root
    }

    deinit {
        _ = fylo_close(root, nil)
    }

    /// The C ABI version this library implements.
    public static var abiVersion: UInt32 { fylo_abi_version() }

    /// Open the root or `.vdbx` database at `path`. `options` is
    /// `{"config": {...}, "identity": {...}, "create": {"mode": ...}}` or
    /// empty; a missing database is made only when `create` says so.
    ///
    /// `key` is an encrypted database's secret, at least 32 bytes: keep it in
    /// the keychain, never in preferences or logs. A short or wrong key
    /// throws `EKEY`. FYLO wipes its own copy.
    public static func open(path: String, options: String = "", key: Data? = nil) throws -> Fylo {
        var root: UInt64 = 0
        var error = FyloBytes()
        let status = FyloInput.text(path) { pathPointer, pathLength in
            FyloInput.text(options) { optionsPointer, optionsLength in
                FyloInput.bytes(key) { keyPointer, keyLength in
                    fylo_open(
                        pathPointer, pathLength, optionsPointer, optionsLength,
                        keyPointer, keyLength, &root, &error)
                }
            }
        }
        try FyloAnswer(status, error).check()
        return Fylo(root: root)
    }

    /// A new encryption key: 32 bytes from the system's secure generator.
    public static func newKey() -> Data {
        var generator = SystemRandomNumberGenerator()
        return Data((0..<32).map { _ in UInt8.random(in: .min ... .max, using: &generator) })
    }

    /// Create a document collection: `{"collection", "kind"}`.
    public func createCollection(_ name: String) throws -> [String: Any] {
        var out = FyloBytes()
        let status = FyloInput.text(name) { fylo_create_collection(root, $0, $1, &out) }
        return try FyloAnswer(status, out).object()
    }

    /// Store a new document from its JSON; answers its id.
    public func put(_ collection: String, json: String) throws -> String {
        try twoTexts(collection, json, fylo_put).value() as? String ?? ""
    }

    /// One document as `{"<id>": {...}}`, or `nil` when it does not exist.
    public func get(_ collection: String, id: String) throws -> [String: Any]? {
        try twoTexts(collection, id, fylo_get).objectOrNil()
    }

    /// Delete one document, keeping its tombstone.
    public func delete(_ collection: String, id: String) throws {
        _ = try twoTexts(collection, id, fylo_delete).value()
    }

    /// Every document a structured query matches: `{"<id>": {...}, ...}`.
    public func find(_ collection: String, query: String) throws -> [String: Any] {
        try twoTexts(collection, query, fylo_find).object()
    }

    /// One page of a query with `$sort` (and `$limit`, `$after`):
    /// `{"items": [{"id", "doc"}], "nextCursor"}`, the cursor null (`NSNull`)
    /// on the last page.
    public func findPage(_ collection: String, query: String) throws -> [String: Any] {
        try twoTexts(collection, query, fylo_find_page).object()
    }

    /// Close the database. Later calls throw with status `.closed`.
    public func close() throws {
        var out = FyloBytes()
        try FyloAnswer(fylo_close(root, &out), out).check()
    }

    private typealias TwoTextCall = (
        UInt64, UnsafePointer<UInt8>?, Int, UnsafePointer<UInt8>?, Int, UnsafeMutablePointer<FyloBytes>?
    ) -> FyloStatus

    private func twoTexts(_ first: String, _ second: String, _ call: TwoTextCall) -> FyloAnswer {
        var out = FyloBytes()
        let status = FyloInput.text(first) { firstPointer, firstLength in
            FyloInput.text(second) { secondPointer, secondLength in
                call(root, firstPointer, firstLength, secondPointer, secondLength, &out)
            }
        }
        return FyloAnswer(status, out)
    }
}
