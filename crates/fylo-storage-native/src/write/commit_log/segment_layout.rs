use std::path::Path;

use serde_json::json;

use super::super::{
    durable_replace, ensure_directory, json_error, sync_directory, write_if_missing,
    write_json_durable,
};
use super::log_segment::LogSegment;
use super::segment_checkpoint::SegmentCheckpoint;
use crate::segments::{DirectoryMedium, SegmentMedium, SlotRecord};
use crate::{NativeRoot, NativeStorageError};

/// Lays out an empty segment collection (ADR 0015 section 2).
///
/// The layout holds the first active segment, opening with a checkpoint
/// record, empty location files, generation slots, an empty prefix index,
/// and a plain file named `docs`, so an older release that looks for the
/// per-document tree finds a file where it expects a directory and refuses.
pub(crate) struct SegmentLayout;

impl SegmentLayout {
    const FENCE: &'static [u8] =
        b"This collection keeps its documents in segments/ (FYLO ADR 0015).\n";
    const FIRST_SEGMENT: u64 = 1;
    const FIRST_LSN: u64 = 1;
    const MARKER_FORMAT: &'static str = "fylo.collection-segments.v1";

    /// Create the layout in `directory`, or complete one a crash left
    /// partly created. An existing slot file means the layout is complete.
    pub(crate) fn create_empty(
        root: &NativeRoot,
        directory: &Path,
        generation: u64,
    ) -> Result<(), NativeStorageError> {
        let medium = DirectoryMedium::of(directory);
        if medium.has_slots()? {
            return Ok(());
        }
        ensure_directory(root, directory)?;
        ensure_directory(root, &directory.join("locks"))?;
        Self::create_index(root, directory)?;
        medium.create_directory(root)?;
        medium.replace_location_snapshot(b"")?;
        medium.reset_location_log()?;
        let slot = Self::create_first_segment(&medium, directory, generation)?;
        medium.create_slots(&slot)?;
        durable_replace(&directory.join("docs"), Self::FENCE)?;
        sync_directory(directory)
    }

    /// Write the fixed `state.json` that makes an older release refuse the
    /// collection instead of reading it as empty.
    pub(crate) fn write_marker(state: &Path) -> Result<(), NativeStorageError> {
        if let Some(parent) = state.parent() {
            fylo_vfs::create_dir_all(parent).map_err(NativeStorageError::io)?;
        }
        let marker = json!({
            "format": Self::MARKER_FORMAT,
            "layout": "segments",
        });
        write_json_durable(state, &marker)
    }

    fn create_index(root: &NativeRoot, directory: &Path) -> Result<(), NativeStorageError> {
        let index = directory.join("index");
        ensure_directory(root, &index)?;
        write_if_missing(
            &index.join("manifest.json"),
            format!(
                "{{\"format\":\"fylo.local-fs.index.v1\",\"createdAt\":{}}}\n",
                super::super::now_millis()?
            )
            .as_bytes(),
        )?;
        write_if_missing(&index.join("keys.snapshot"), b"")?;
        write_if_missing(&index.join("keys.wal"), b"")
    }

    fn create_first_segment(
        medium: &dyn SegmentMedium,
        directory: &Path,
        generation: u64,
    ) -> Result<SlotRecord, NativeStorageError> {
        let manifest = SegmentCheckpoint::manifest(directory, medium)?;
        let bytes = serde_json::to_vec(&manifest).map_err(|error| json_error(&error))?;
        medium.remove_segment(Self::FIRST_SEGMENT)?;
        let mut segment = LogSegment::create_documents(
            medium,
            (Self::FIRST_SEGMENT, Self::FIRST_LSN),
            bytes,
            generation,
        )?;
        segment.full_flush()?;
        Ok(SlotRecord {
            generation,
            writing: false,
            segment: Self::FIRST_SEGMENT,
            length: segment.length(),
            next_lsn: segment.next_lsn(),
            records: 0,
            locations_length: 0,
            table_epoch: 0,
        })
    }
}
