import CFylo
import Foundation

/// One answer of a C call: its status and the JSON FYLO gave back. The
/// buffer is copied and returned to `fylo_bytes_free` at once.
struct FyloAnswer {
    let status: FyloCallStatus
    let json: Data

    init(_ status: FyloStatus, _ bytes: FyloBytes) {
        self.status = FyloCallStatus(status)
        self.json = FyloAnswer.copy(bytes)
        fylo_bytes_free(bytes)
    }

    /// Throw the error of a call that did not work; a call that worked may
    /// answer no JSON at all, as `fylo_open` does.
    func check() throws {
        try requireOk()
    }

    /// The JSON value of a call that worked.
    func value() throws -> Any {
        try requireOk()
        return try JSONSerialization.jsonObject(with: json, options: [.fragmentsAllowed])
    }

    /// The JSON object of a call that worked.
    func object() throws -> [String: Any] {
        try value() as? [String: Any] ?? [:]
    }

    /// The JSON object, or `nil` when FYLO answered "not found".
    func objectOrNil() throws -> [String: Any]? {
        if status == .notFound { return nil }
        return try object()
    }

    private func requireOk() throws {
        if status == .ok { return }
        throw failure()
    }

    private func failure() -> FyloError {
        let error = (try? JSONSerialization.jsonObject(with: json)) as? [String: Any] ?? [:]
        return FyloError(
            status: status,
            code: error["code"] as? String ?? "",
            message: error["message"] as? String ?? ""
        )
    }

    private static func copy(_ bytes: FyloBytes) -> Data {
        guard let pointer = bytes.ptr else { return Data() }
        return Data(bytes: pointer, count: bytes.len)
    }
}
