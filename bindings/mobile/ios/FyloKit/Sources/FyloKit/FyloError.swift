import Foundation

/// A FYLO call that did not work. `code` is a code of FYLO's error list
/// (`api/errors/v1.json`), such as `EROOTCLOSED`, `EBADREQUEST` or `EKEY`.
public struct FyloError: Error, CustomStringConvertible {
    public let status: FyloCallStatus
    public let code: String
    public let message: String

    public var description: String { "\(code): \(message)" }
}
