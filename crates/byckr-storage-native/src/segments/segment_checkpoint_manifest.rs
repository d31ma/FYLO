use serde::{Deserialize, Serialize};

/// The manifest of a checkpoint record in a document segment.
///
/// It records the derived files' durable state, so recovery can cut their
/// logs back to it and replay every later record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SegmentCheckpointManifest {
    pub(crate) format: String,
    pub(crate) locations_wal_length: u64,
    pub(crate) locations_snapshot_sha256: String,
    pub(crate) keys_wal_length: u64,
    pub(crate) snapshot_sha256: String,
    /// Digest of the schemas snapshot (ADR 0017 section 3), absent when the
    /// collection has none. A checkpoint written before schemas snapshots
    /// existed has no such field, which reads as no snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) schemas_snapshot_sha256: Option<String>,
}

impl SegmentCheckpointManifest {
    pub(crate) const FORMAT: &'static str = "fylo.segment-checkpoint.v1";
}
