import CFylo
import Foundation

/// Application identities (ADR 0021): keys named by a key reference.
public enum FyloIdentity {
    /// Make the key `reference` names: `{"id", "publicKey"}`.
    public static func create(_ reference: String) throws -> [String: Any] {
        var out = FyloBytes()
        let status = FyloInput.text(reference) { fylo_identity_create($0, $1, &out) }
        return try FyloAnswer(status, out).object()
    }

    /// Describe the identity `reference` names: `{"id", "publicKey"}`.
    public static func show(_ reference: String) throws -> [String: Any] {
        var out = FyloBytes()
        let status = FyloInput.text(reference) { fylo_identity_show($0, $1, &out) }
        return try FyloAnswer(status, out).object()
    }
}
