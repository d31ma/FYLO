use serde::{Deserialize, Serialize};

use super::SegmentEntry;
use crate::{NativeStorageError, NativeStorageErrorCode};

/// The manifest of a commit record in a document segment.
///
/// Compaction writes the same manifest, one entry per record, with the
/// operation `compaction`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SegmentCommitManifest {
    pub(crate) format: String,
    pub(crate) operation: String,
    pub(crate) entries: Vec<SegmentEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) index_removed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) index_added: Vec<String>,
}

impl SegmentCommitManifest {
    pub(crate) const FORMAT: &'static str = "fylo.segment-commit.v1";

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, NativeStorageError> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                format!("segment commit manifest is corrupt: {error}"),
            )
        })?;
        if manifest.format != Self::FORMAT {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "segment commit manifest has an unknown format",
            ));
        }
        Ok(manifest)
    }
}
