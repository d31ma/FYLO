use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use fylo_format::LogRecord;
use fylo_vfs::OpenOptions;

use super::logged_plan::LoggedPlan;
use super::placed_record::PlacedRecord;
use crate::segments::{
    LocationState, SegmentBody, SegmentCommitManifest, SegmentEntry, SegmentLocation, SegmentPaths,
};
use crate::write::{NativeWriteRoot, failpoint, json_error};
use crate::{NativeCollection, NativeRoot, NativeStorageError, NativeStorageErrorCode};

/// One mutation of a segment collection, as the entries its commit record
/// will hold.
///
/// Applying the plan writes no document file: the record in the active
/// segment is the document. It only appends each entry's new location and
/// the index delta.
#[derive(Debug)]
pub(crate) struct SegmentPlan {
    operation: String,
    entries: Vec<SegmentEntry>,
    blobs: Vec<u8>,
    index_removed: BTreeSet<String>,
    index_added: BTreeSet<String>,
    previous: Vec<Option<SegmentLocation>>,
    touched: BTreeSet<PathBuf>,
}

impl SegmentPlan {
    const BLOB_BUDGET: usize = LogRecord::MAX_BODY_BYTES - 1024 * 1024;
    const MAX_ENTRIES: usize = u16::MAX as usize;

    pub(crate) fn new(operation: &str) -> Self {
        Self {
            operation: operation.into(),
            entries: Vec::new(),
            blobs: Vec::new(),
            index_removed: BTreeSet::new(),
            index_added: BTreeSet::new(),
            previous: Vec::new(),
            touched: BTreeSet::new(),
        }
    }

    /// Add one document's post-image. `entry.body` is set from `body`, and
    /// `previous` is where the document's newest entry was, for rollback.
    pub(crate) fn push(
        &mut self,
        collection: &Path,
        (mut entry, body): (SegmentEntry, Option<&[u8]>),
        previous: Option<SegmentLocation>,
    ) -> Result<(), NativeStorageError> {
        entry.body = body.map(|bytes| self.add_blob(bytes));
        self.require_within_budget()?;
        self.touched
            .insert(Self::virtual_path(collection, &entry.id));
        self.entries.push(entry);
        self.previous.push(previous);
        Ok(())
    }

    /// Record the index keys the mutation removes and adds.
    pub(crate) fn index_delta(&mut self, before: &BTreeSet<String>, after: &BTreeSet<String>) {
        self.index_removed.extend(before.difference(after).cloned());
        self.index_added.extend(after.difference(before).cloned());
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The path a busy period uses to keep two members off one document.
    fn virtual_path(collection: &Path, id: &str) -> PathBuf {
        collection.join("segments").join(id)
    }

    fn add_blob(&mut self, bytes: &[u8]) -> SegmentBody {
        let offset = self.blobs.len() as u64;
        self.blobs.extend_from_slice(bytes);
        SegmentBody {
            offset,
            length: bytes.len() as u64,
            sha256: crate::sha256_hex(bytes),
        }
    }

    fn require_within_budget(&self) -> Result<(), NativeStorageError> {
        if self.blobs.len() > Self::BLOB_BUDGET || self.entries.len() >= Self::MAX_ENTRIES {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "mutation exceeds the segment record limit",
            ));
        }
        Ok(())
    }

    fn rows(&self, placed: &PlacedRecord) -> Vec<SegmentLocation> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, entry)| SegmentLocation {
                id: entry.id.clone(),
                segment: placed.segment,
                offset: placed.offset,
                length: placed.length,
                entry: u16::try_from(index).unwrap_or(u16::MAX),
                state: entry.state,
                lsn: placed.lsn,
            })
            .collect()
    }

    /// Rows that put every touched document back where it was. A document
    /// that had no entry gets an undo row, which readers skip.
    fn undo_rows(&self) -> Vec<SegmentLocation> {
        self.entries
            .iter()
            .zip(&self.previous)
            .map(|(entry, previous)| {
                previous.clone().unwrap_or_else(|| SegmentLocation {
                    id: entry.id.clone(),
                    segment: 0,
                    offset: 0,
                    length: 0,
                    entry: 0,
                    state: LocationState::Absent,
                    lsn: 0,
                })
            })
            .collect()
    }

    /// Append location rows to the collection's location log.
    pub(crate) fn append_locations(
        collection: &Path,
        rows: &[SegmentLocation],
    ) -> Result<(), NativeStorageError> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(rows.len() * SegmentLocation::ENCODED_BYTES);
        for row in rows {
            bytes.extend_from_slice(&row.encode()?);
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(SegmentPaths::of(collection).locations_wal())
            .map_err(NativeStorageError::io)?;
        file.write_all(&bytes).map_err(NativeStorageError::io)
    }
}

impl LoggedPlan for SegmentPlan {
    fn touched(&self) -> &BTreeSet<PathBuf> {
        &self.touched
    }

    fn manifest(&self) -> Result<Vec<u8>, NativeStorageError> {
        let manifest = SegmentCommitManifest {
            format: SegmentCommitManifest::FORMAT.into(),
            operation: self.operation.clone(),
            entries: self.entries.clone(),
            index_removed: self.index_removed.iter().cloned().collect(),
            index_added: self.index_added.iter().cloned().collect(),
        };
        serde_json::to_vec(&manifest).map_err(|error| json_error(&error))
    }

    fn blobs(&self) -> &[u8] {
        &self.blobs
    }

    fn apply(
        &self,
        _root: &NativeRoot,
        collection: &NativeCollection,
        placed: &PlacedRecord,
    ) -> Result<(), (NativeStorageError, bool)> {
        Self::append_locations(&collection.path, &self.rows(placed))
            .map_err(|error| (error, false))?;
        let delta = NativeWriteRoot::index_lines(&self.index_removed, &self.index_added);
        NativeWriteRoot::append_index(collection, &delta).map_err(|error| (error, true))?;
        failpoint("after-log-apply-step").map_err(|error| (error, true))
    }

    fn undo(
        &self,
        _root: &NativeRoot,
        collection: &NativeCollection,
        index_applied: bool,
    ) -> Result<(), NativeStorageError> {
        Self::append_locations(&collection.path, &self.undo_rows())?;
        if index_applied {
            let inverse = NativeWriteRoot::index_lines(&self.index_added, &self.index_removed);
            NativeWriteRoot::append_index(collection, &inverse)?;
        }
        Ok(())
    }
}
