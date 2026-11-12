import FyloKit
import XCTest

/// The Swift wrapper on the iOS simulator, through the C ABI (ADR 0026).
final class FyloKitTests: XCTestCase {
    private var folder: URL!

    override func setUpWithError() throws {
        folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("fylo-test-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: folder)
    }

    private var database: String { folder.appendingPathComponent("app.vdbx").path }

    private func openPlain() throws -> Fylo {
        try Fylo.open(path: database, options: #"{"create": {"mode": "plain"}}"#)
    }

    private func expectStatus(_ expected: FyloCallStatus, _ call: () throws -> Void) {
        XCTAssertThrowsError(try call()) { error in
            XCTAssertEqual((error as? FyloError)?.status, expected)
        }
    }

    func testTheLibraryReportsAbiVersion1() {
        XCTAssertEqual(Fylo.abiVersion, 1)
    }

    func testAPlainDatabaseIsCreatedWrittenReadFoundAndDeleted() throws {
        let db = try openPlain()
        XCTAssertEqual(try db.createCollection("notes")["collection"] as? String, "notes")
        let id = try db.put("notes", json: #"{"title": "Hello", "rank": 1}"#)
        let note = try db.get("notes", id: id)?[id] as? [String: Any]
        XCTAssertEqual(note?["title"] as? String, "Hello")
        let found = try db.find("notes", query: #"{"$ops": [{"title": {"$eq": "Hello"}}]}"#)
        XCTAssertNotNil(found[id])
        try db.delete("notes", id: id)
        XCTAssertNil(try db.get("notes", id: id))
        try db.close()
    }

    func testSortedPagesFollowTheCursorToTheLastPage() throws {
        let db = try openPlain()
        _ = try db.createCollection("messages")
        for sent in 1...3 { _ = try db.put("messages", json: #"{"sentAt": \#(sent)}"#) }
        let first = try db.findPage("messages", query: #"{"$sort": {"sentAt": "desc"}, "$limit": 2}"#)
        XCTAssertEqual((first["items"] as? [Any])?.count, 2)
        let cursor = try XCTUnwrap(first["nextCursor"] as? String)
        let second = try db.findPage(
            "messages",
            query: #"{"$sort": {"sentAt": "desc"}, "$limit": 2, "$after": "\#(cursor)"}"#)
        XCTAssertEqual((second["items"] as? [Any])?.count, 1)
        XCTAssertTrue(second["nextCursor"] is NSNull)
        try db.close()
    }

    func testAClosedDatabaseAnswersClosed() throws {
        let db = try openPlain()
        try db.close()
        expectStatus(.closed) { _ = try db.createCollection("late") }
    }

    func testAMissingDatabaseWithoutCreateIsNotMade() {
        expectStatus(.error) { _ = try Fylo.open(path: database) }
        XCTAssertFalse(FileManager.default.fileExists(atPath: database))
    }

    func testBadOptionsAreAnInvalidArgument() {
        expectStatus(.invalidArgument) { _ = try Fylo.open(path: database, options: #"{"unknown": true}"#) }
    }

    func testAnEncryptedDatabaseOpensOnlyWithItsKey() throws {
        let key = Fylo.newKey()
        let db = try Fylo.open(path: database, options: #"{"create": {"mode": "encrypted"}}"#, key: key)
        _ = try db.createCollection("notes")
        let id = try db.put("notes", json: #"{"secret": "lighthouse"}"#)
        try db.close()
        let reopened = try Fylo.open(path: database, key: key)
        let note = try reopened.get("notes", id: id)?[id] as? [String: Any]
        XCTAssertEqual(note?["secret"] as? String, "lighthouse")
        try reopened.close()
        XCTAssertThrowsError(try Fylo.open(path: database, key: Fylo.newKey())) { error in
            XCTAssertEqual((error as? FyloError)?.code, "EKEY")
        }
        let bytes = try Data(contentsOf: URL(fileURLWithPath: database))
        XCTAssertNil(bytes.range(of: Data("lighthouse".utf8)))
    }

    func testAShortKeyIsRefused() {
        XCTAssertThrowsError(
            try Fylo.open(path: database, options: #"{"create": {"mode": "encrypted"}}"#, key: Data(count: 31))
        ) { error in
            XCTAssertEqual((error as? FyloError)?.code, "EKEY")
        }
    }

    func testAFileIdentityProtectsADatabase() throws {
        let reference = "file:" + folder.appendingPathComponent("device.pem").path
        let created = try FyloIdentity.create(reference)
        XCTAssertEqual(created["id"] as? String, try FyloIdentity.show(reference)["id"] as? String)
        let db = try Fylo.open(
            path: database,
            options: #"{"create": {"mode": "protected", "admin": "\#(reference)"}}"#)
        _ = try db.createCollection("secrets")
        let id = try db.put("secrets", json: #"{"pin": "1234"}"#)
        let secret = try db.get("secrets", id: id)?[id] as? [String: Any]
        XCTAssertEqual(secret?["pin"] as? String, "1234")
        try db.close()
    }
}
