import CFylo

/// What a FYLO call did; the numbers of `FyloStatus` in `fylo.h`.
public enum FyloCallStatus: UInt32, Sendable {
    case ok = 0
    case notFound = 1
    case error = 2
    case invalidArgument = 3
    case closed = 4
    case panic = 5

    init(_ status: FyloStatus) {
        self = FyloCallStatus(rawValue: status.rawValue) ?? .panic
    }
}
