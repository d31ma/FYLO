import Foundation

/// The caller's text or bytes as the pointer and length a C call borrows.
enum FyloInput {
    static func text<Result>(
        _ value: String,
        _ body: (UnsafePointer<UInt8>?, Int) -> Result
    ) -> Result {
        let bytes = Array(value.utf8)
        return bytes.withUnsafeBufferPointer { body($0.baseAddress, $0.count) }
    }

    static func bytes<Result>(
        _ value: Data?,
        _ body: (UnsafePointer<UInt8>?, Int) -> Result
    ) -> Result {
        guard let value else { return body(nil, 0) }
        return value.withUnsafeBytes { raw in
            body(raw.bindMemory(to: UInt8.self).baseAddress, raw.count)
        }
    }
}
