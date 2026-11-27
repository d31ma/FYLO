use serde::{Deserialize, Serialize};

/// The manifest of the checkpoint record that opens every segment.
///
/// It records the durable index state the segment's commits build on, so
/// recovery can cut the index log back to it and replay from there.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CheckpointManifest {
    pub(crate) format: String,
    pub(crate) keys_wal_length: u64,
    pub(crate) snapshot_sha256: String,
}

impl CheckpointManifest {
    pub(crate) const FORMAT: &'static str = "fylo.log-checkpoint.v1";
}
