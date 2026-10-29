use serde::{Deserialize, Serialize};

use super::log_segment::LogSegment;
use super::segment_rebuild::SegmentRebuild;
use crate::segments::{LocationTable, SegmentLocation, SegmentPaths};
use crate::{NativeStorageError, NativeStorageErrorCode};

/// The manifest of a compaction record: the compacted segment a collection
/// adopts and the segments it replaces (ADR 0015 section 7).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CompactionManifest {
    pub(crate) format: String,
    pub(crate) segment: u64,
    pub(crate) sha256: String,
    pub(crate) replaces: Vec<u64>,
}

impl CompactionManifest {
    pub(crate) const FORMAT: &'static str = "fylo.segment-compaction.v1";

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, NativeStorageError> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                format!("segment compaction manifest is corrupt: {error}"),
            )
        })?;
        if manifest.format != Self::FORMAT {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "segment compaction manifest has an unknown format",
            ));
        }
        Ok(manifest)
    }

    /// Location rows that move each document whose newest entry the
    /// compacted segment copied to that copy. A copy whose document was
    /// written again after the copy was taken is dead on arrival.
    pub(crate) fn adopted_rows(
        paths: &SegmentPaths,
        table: &LocationTable,
        segment: u64,
    ) -> Result<Vec<SegmentLocation>, NativeStorageError> {
        let scan = LogSegment::scan(&paths.segment(segment))?.ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptDocument,
                format!("compacted segment {segment} has no valid header"),
            )
        })?;
        let lengths = SegmentRebuild::frame_lengths(&scan);
        let mut rows = Vec::new();
        for ((offset, record), length) in scan.records.iter().zip(lengths) {
            for row in SegmentRebuild::rows_of(segment, (*offset, length), record)? {
                if table
                    .get(&row.id)
                    .is_some_and(|current| current.lsn == row.lsn)
                {
                    rows.push(row);
                }
            }
        }
        Ok(rows)
    }
}
