use serde::{Deserialize, Serialize};

/// Where a collection's write-ahead log ended when its generation was last
/// published.
///
/// A reader that finds the log longer than this, or gone, knows a committed
/// record has not been applied yet and treats the collection as writing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct LogPosition {
    /// Random identity of the segment, from its header.
    pub nonce: String,
    /// Byte length of the segment.
    pub length: u64,
    /// Sequence number the next record takes.
    pub next_lsn: u64,
    /// Commit and abort records in the segment.
    pub records: u64,
}
