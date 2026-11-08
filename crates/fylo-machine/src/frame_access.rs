/// Whether a frame only reads the root, so the session's hot set may answer
/// it and need not be cleared around it.
///
/// Only operations known to read through the read-only engine qualify. Any
/// other frame, including one without an operation, is treated as a write:
/// the session clears the hot set before and after it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FrameAccess {
    Read,
    Write,
}

impl FrameAccess {
    const READ_OPERATIONS: [&'static str; 12] = [
        "handshake",
        "getDoc",
        "getLatest",
        "getFileData",
        "getMeta",
        "findDocs",
        "findDeletedDocs",
        "joinDocs",
        "inspectCollection",
        "inspectDatabase",
        "verifyDatabase",
        "log",
    ];

    pub(crate) fn of(operation: Option<&str>) -> Self {
        if operation.is_some_and(|operation| Self::READ_OPERATIONS.contains(&operation)) {
            return Self::Read;
        }
        Self::Write
    }
}
