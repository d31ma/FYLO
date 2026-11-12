import FyloKit
import Security
import XCTest

/// `ios-keychain:` identities through FYLO (ADR 0026, section 8): a P-256
/// key kept in the Data Protection keychain on this device only.
final class FyloKeychainTests: XCTestCase {
    private var folder: URL!
    private let label = "fylo.test.\(UUID().uuidString)"
    private var reference: String { "ios-keychain:\(label)" }

    override func setUpWithError() throws {
        folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("fylo-keychain-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: label,
            kSecAttrAccount as String: "fylo-identity",
            kSecUseDataProtectionKeychain as String: true,
        ]
        SecItemDelete(query as CFDictionary)
        try? FileManager.default.removeItem(at: folder)
    }

    func testAKeychainIdentityIsCreatedAndShownAgain() throws {
        let created = try FyloIdentity.create(reference)
        XCTAssertEqual(created["id"] as? String, try FyloIdentity.show(reference)["id"] as? String)
    }

    func testAKeychainIdentityProtectsADatabaseAcrossReopening() throws {
        let id = try XCTUnwrap(FyloIdentity.create(reference)["id"] as? String)
        let database = folder.appendingPathComponent("secure.vdbx").path
        let db = try Fylo.open(
            path: database,
            options: #"{"create": {"mode": "protected", "admin": "\#(reference)"}}"#)
        _ = try db.createCollection("secrets")
        let stored = try db.put("secrets", json: #"{"pin": "1234"}"#)
        try db.close()
        let reopened = try Fylo.open(
            path: database,
            options: #"{"identity": {"reference": "\#(reference)", "trust": {"pinned": "\#(id)"}}}"#)
        let secret = try reopened.get("secrets", id: stored)?[stored] as? [String: Any]
        XCTAssertEqual(secret?["pin"] as? String, "1234")
        try reopened.close()
    }

    func testASecondKeyUnderTheSameLabelIsRefused() throws {
        _ = try FyloIdentity.create(reference)
        XCTAssertThrowsError(try FyloIdentity.create(reference)) { error in
            XCTAssertEqual((error as? FyloError)?.status, .error)
        }
    }
}
