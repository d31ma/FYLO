use crate::RequestFrame;

/// Whether a frame only reads the root, and so may run beside other reads.
///
/// Only operations known to read through the read-only engine qualify. Any
/// other frame, including one that cannot be parsed, is treated as a write:
/// it runs alone and clears the hot set before and after.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FrameAccess {
    Read,
    Write,
}

impl FrameAccess {
    const READ_OPERATIONS: [&'static str; 10] = [
        "handshake",
        "getDoc",
        "getLatest",
        "getFileData",
        "getMeta",
        "findDocs",
        "findDeletedDocs",
        "joinDocs",
        "inspectCollection",
        "log",
    ];

    pub(crate) fn of(frame: &RequestFrame) -> Self {
        let operation = frame.operation();
        Self::for_operation(operation.as_deref())
    }

    fn for_operation(operation: Option<&str>) -> Self {
        if operation.is_some_and(|operation| Self::READ_OPERATIONS.contains(&operation)) {
            return Self::Read;
        }
        Self::Write
    }
}
