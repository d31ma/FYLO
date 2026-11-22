use serde::{Deserialize, Serialize};

use serde::de::DeserializeSeed;

use super::{ManifestEntrySeed, SegmentEntry};
use crate::{NativeStorageError, NativeStorageErrorCode, SegmentReadLog};

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
        let manifest: Self = serde_json::from_slice(bytes).map_err(Self::corrupt)?;
        Self::require_format(&manifest.format)?;
        SegmentReadLog::record_entries(manifest.entries.len());
        Ok(manifest)
    }

    /// The entry at `index` of the manifest `bytes` hold, building only that
    /// entry: the others are skipped, not decoded. The format and the JSON
    /// as a whole are checked as [`Self::decode`] checks them. `None` when
    /// the manifest has no entry at `index`.
    pub(crate) fn entry_at(
        bytes: &[u8],
        index: u16,
    ) -> Result<Option<SegmentEntry>, NativeStorageError> {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let (format, entry) = ManifestEntrySeed::new(usize::from(index))
            .deserialize(&mut deserializer)
            .and_then(|found| deserializer.end().map(|()| found))
            .map_err(Self::corrupt)?;
        Self::require_format(&format)?;
        SegmentReadLog::record_entries(usize::from(entry.is_some()));
        Ok(entry)
    }

    fn require_format(format: &str) -> Result<(), NativeStorageError> {
        if format != Self::FORMAT {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "segment commit manifest has an unknown format",
            ));
        }
        Ok(())
    }

    fn corrupt(error: serde_json::Error) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::CorruptMetadata,
            format!("segment commit manifest is corrupt: {error}"),
        )
    }
}
