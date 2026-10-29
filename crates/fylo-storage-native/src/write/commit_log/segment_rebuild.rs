use std::collections::{BTreeMap, BTreeSet};

use fylo_format::{LogRecordKind, LogScan};

use super::super::{NativeWriteRoot, durable_replace};
use super::abort_manifest::AbortManifest;
use super::log_segment::LogSegment;
use super::segment_schemas::SegmentSchemas;
use crate::segments::{
    LocationState, LocationTable, SegmentCommitManifest, SegmentLocation, SegmentMedium,
    SegmentStore, TableCache,
};
use crate::{NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Rebuilds every derived file of a segment collection from its segments
/// alone: documents and schema records are the truth, the location table,
/// the schemas snapshot and the prefix index are accelerators (ADR 0015
/// section 8, ADR 0017 section 3).
///
/// For each document the entry with the highest sequence number wins, an
/// entry an abort record cancelled never wins, and between identical copies
/// (a compaction's) the higher segment number is kept.
pub(crate) struct SegmentRebuild;

impl SegmentRebuild {
    const INDEX_BATCH: usize = 256;

    /// Rebuild the location table, the schemas snapshot and the index. Runs
    /// under the collection lock.
    pub(crate) fn run(collection: &NativeCollection) -> Result<(), NativeStorageError> {
        let medium = collection.segment_medium()?;
        let scans = Self::scan_all(&*medium)?;
        SegmentSchemas::rebuild(&scans)?.write_if_changed(&*medium)?;
        let table = LocationTable::from_rows(Self::winners(&scans)?);
        medium.replace_location_snapshot(&table.encode_snapshot()?)?;
        medium.reset_location_log()?;
        TableCache::shared().forget(&*medium);
        Self::rebuild_index(collection)
    }

    /// Rebuild only the prefix index from the documents the location log
    /// already names.
    pub(crate) fn rebuild_index(collection: &NativeCollection) -> Result<(), NativeStorageError> {
        let store = SegmentStore::open_for_writer(collection.segment_medium()?)?;
        let mut keys = BTreeSet::new();
        for batch in store.ids(LocationState::Live).chunks(Self::INDEX_BATCH) {
            for document in store.documents(batch, LocationState::Live)? {
                keys.extend(NativeWriteRoot::document_keys(
                    &document.entry.id,
                    &document.body,
                )?);
            }
        }
        let mut snapshot = Vec::new();
        for key in keys {
            snapshot.extend_from_slice(key.as_bytes());
            snapshot.push(b'\n');
        }
        let medium = collection.segment_medium()?;
        if medium.keeps_index() {
            return medium.replace_index_snapshot(&snapshot);
        }
        let index = collection.path.join("index");
        durable_replace(&index.join("keys.snapshot"), &snapshot)?;
        durable_replace(&index.join("keys.wal"), b"")
    }

    /// Scan every segment present, with its number.
    pub(crate) fn scan_all(
        medium: &dyn SegmentMedium,
    ) -> Result<Vec<(u64, LogScan)>, NativeStorageError> {
        let mut scans = Vec::new();
        for number in medium.segment_numbers()? {
            let scan = LogSegment::scan_segment(medium, number)?.ok_or_else(|| {
                NativeStorageError::new(
                    NativeStorageErrorCode::CorruptDocument,
                    format!("segment {number} has no valid header; run fylo salvage"),
                )
            })?;
            scans.push((number, scan));
        }
        Ok(scans)
    }

    /// Byte length of each record of a scan, in record order.
    pub(crate) fn frame_lengths(scan: &LogScan) -> Vec<u32> {
        let mut ends: Vec<u64> = scan
            .records
            .iter()
            .skip(1)
            .map(|(offset, _)| *offset)
            .collect();
        ends.push(scan.valid_length);
        scan.records
            .iter()
            .zip(ends)
            .map(|((offset, _), end)| u32::try_from(end - offset).unwrap_or(u32::MAX))
            .collect()
    }

    /// The sequence numbers abort records cancelled, across all scans.
    pub(crate) fn aborted<'a>(
        scans: impl IntoIterator<Item = &'a LogScan>,
    ) -> Result<BTreeSet<u64>, NativeStorageError> {
        let mut aborted = BTreeSet::new();
        for scan in scans {
            for (_, record) in &scan.records {
                if record.kind != LogRecordKind::Abort {
                    continue;
                }
                let manifest: AbortManifest = serde_json::from_slice(&record.manifest)
                    .map_err(|error| Self::corrupt(&error.to_string()))?;
                aborted.insert(manifest.aborts);
            }
        }
        Ok(aborted)
    }

    /// The location rows one commit record produces.
    pub(crate) fn rows_of(
        number: u64,
        (offset, length): (u64, u32),
        record: &fylo_format::LogRecord,
    ) -> Result<Vec<SegmentLocation>, NativeStorageError> {
        let manifest = SegmentCommitManifest::decode(&record.manifest)?;
        manifest
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                Ok(SegmentLocation {
                    id: entry.id.clone(),
                    segment: number,
                    offset,
                    length,
                    entry: u16::try_from(index).map_err(|_| Self::corrupt("too many entries"))?,
                    state: entry.state,
                    lsn: record.lsn,
                })
            })
            .collect()
    }

    fn winners(scans: &[(u64, LogScan)]) -> Result<Vec<SegmentLocation>, NativeStorageError> {
        let aborted = Self::aborted(scans.iter().map(|(_, scan)| scan))?;
        let mut winners: BTreeMap<String, SegmentLocation> = BTreeMap::new();
        for (number, scan) in scans {
            let lengths = Self::frame_lengths(scan);
            for ((offset, record), length) in scan.records.iter().zip(lengths) {
                if record.kind != LogRecordKind::Commit || aborted.contains(&record.lsn) {
                    continue;
                }
                for row in Self::rows_of(*number, (*offset, length), record)? {
                    Self::keep_newest(&mut winners, row);
                }
            }
        }
        Ok(winners.into_values().collect())
    }

    fn keep_newest(winners: &mut BTreeMap<String, SegmentLocation>, row: SegmentLocation) {
        let newer = winners
            .get(&row.id)
            .is_none_or(|current| (row.lsn, row.segment) >= (current.lsn, current.segment));
        if newer {
            winners.insert(row.id.clone(), row);
        }
    }

    fn corrupt(detail: &str) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::CorruptMetadata,
            format!("segment record is corrupt: {detail}"),
        )
    }
}
