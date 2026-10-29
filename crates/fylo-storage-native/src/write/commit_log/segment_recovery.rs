use std::collections::BTreeSet;

use fylo_format::{LogRecordKind, LogScan, LogSegmentKind};
use fylo_vfs::OpenOptions;

use super::super::NativeWriteRoot;
use super::compaction_manifest::CompactionManifest;
use super::log_segment::LogSegment;
use super::segment_checkpoint::SegmentCheckpoint;
use super::segment_plan::SegmentPlan;
use super::segment_rebuild::SegmentRebuild;
use crate::segments::{
    LocationTable, SegmentCheckpointManifest, SegmentCommitManifest, SegmentLocation,
    SegmentMedium, TableCache, TableReach,
};
use crate::{GenerationState, NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Brings a segment collection back to one stable state after a crash
/// (ADR 0015 section 8).
///
/// Recovery is redo-only. It cuts the active segment at its first invalid
/// record (an append nobody was told succeeded), cuts the location and index
/// logs back to the last checkpoint, and replays every later commit and
/// compaction. When a derived file no longer matches what the checkpoint
/// recorded, both are rebuilt from segments. Segments that no location
/// refers to any more (a compaction's inputs or an unadopted output) are
/// removed, and a checkpoint closes the recovery.
pub(crate) struct SegmentRecovery;

impl SegmentRecovery {
    pub(crate) fn run(
        _writer: &NativeWriteRoot,
        collection: &NativeCollection,
        stored: &GenerationState,
    ) -> Result<(), NativeStorageError> {
        let medium = collection.segment_medium()?;
        let (number, scan) = Self::active_scan(&*medium, stored)?;
        Self::require_torn_tail_only(&*medium, number, &scan)?;
        let seen = scan
            .records
            .iter()
            .map(|(_, record)| record.generation)
            .max()
            .unwrap_or(0);
        let next_lsn = scan
            .records
            .last()
            .map_or(scan.header.first_lsn, |(_, record)| record.lsn + 1);
        let segment = LogSegment::open_scanned(&*medium, (number, scan.valid_length), next_lsn)?;
        let generation = SegmentCheckpoint::bump_epoch(
            collection,
            &segment,
            SegmentCheckpoint::next_stable(stored, seen),
        )?;
        if !Self::replay_from_checkpoint(collection, &*medium, (number, &scan))? {
            SegmentRebuild::run(collection)?;
        }
        TableCache::shared().forget(&*medium);
        Self::remove_unreferenced(&*medium, number)?;
        SegmentCheckpoint::run(collection, segment, generation)?;
        Ok(())
    }

    /// The active segment and its valid records. It is the segment the
    /// newest slot names, or else the highest-numbered segment a commit
    /// (not a compaction) wrote.
    fn active_scan(
        medium: &dyn SegmentMedium,
        stored: &GenerationState,
    ) -> Result<(u64, LogScan), NativeStorageError> {
        let named = stored
            .log
            .as_ref()
            .and_then(|position| u64::from_str_radix(&position.nonce, 16).ok());
        let mut candidates: Vec<u64> = medium.segment_numbers()?;
        candidates.reverse();
        if let Some(named) = named {
            candidates.insert(0, named);
        }
        for number in candidates {
            if let Some(scan) = LogSegment::scan_segment(medium, number)?
                && scan.header.kind == LogSegmentKind::Documents
            {
                return Ok((number, scan));
            }
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::CorruptMetadata,
            "segment collection has no active segment",
        ))
    }

    /// Recovery cuts the active segment where its valid records stop. That
    /// is right for an interrupted append, which leaves nothing readable
    /// after it. A readable record further on means damage in the middle of
    /// committed data, and cutting there would lose it, so recovery stops
    /// and asks for a salvage instead.
    fn require_torn_tail_only(
        medium: &dyn SegmentMedium,
        number: u64,
        scan: &LogScan,
    ) -> Result<(), NativeStorageError> {
        let bytes = medium.require_segment(number)?;
        if scan.valid_length >= bytes.len() as u64 {
            return Ok(());
        }
        let readable_later = fylo_format::LogSalvage::segment(&bytes).is_ok_and(|salvage| {
            salvage
                .records
                .iter()
                .any(|(offset, _)| *offset > scan.valid_length)
        });
        if readable_later {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptDocument,
                format!(
                    "segment {number} is damaged at byte {} with committed records after it; run fylo salvage",
                    scan.valid_length
                ),
            ));
        }
        Ok(())
    }

    /// Replay every record after the last checkpoint. Returns false when
    /// there is no checkpoint, or the derived files no longer match it.
    fn replay_from_checkpoint(
        collection: &NativeCollection,
        medium: &dyn SegmentMedium,
        (number, scan): (u64, &LogScan),
    ) -> Result<bool, NativeStorageError> {
        let Some((start, checkpoint)) = Self::last_checkpoint(scan) else {
            return Ok(false);
        };
        if !Self::cut_back(collection, medium, &checkpoint)? {
            return Ok(false);
        }
        let aborted = SegmentRebuild::aborted([scan])?;
        let reach = TableReach::checkpoint(medium, checkpoint.locations_wal_length);
        let mut table = LocationTable::load(medium, reach)?;
        let lengths = SegmentRebuild::frame_lengths(scan);
        for (index, (offset, record)) in scan.records.iter().enumerate().skip(start + 1) {
            let rows = match record.kind {
                LogRecordKind::Commit if !aborted.contains(&record.lsn) => {
                    Self::replay_commit(collection, (number, *offset, lengths[index]), record)?
                }
                LogRecordKind::Compaction if !aborted.contains(&record.lsn) => {
                    Self::replay_compaction(medium, &table, record)?
                }
                _ => Vec::new(),
            };
            SegmentPlan::append_locations(medium, &rows)?;
            for row in rows {
                table.apply(row);
            }
        }
        Ok(true)
    }

    fn last_checkpoint(scan: &LogScan) -> Option<(usize, SegmentCheckpointManifest)> {
        scan.records
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (_, record))| record.kind == LogRecordKind::Checkpoint)
            .and_then(|(index, (_, record))| {
                serde_json::from_slice::<SegmentCheckpointManifest>(&record.manifest)
                    .ok()
                    .filter(|manifest| manifest.format == SegmentCheckpointManifest::FORMAT)
                    .map(|manifest| (index, manifest))
            })
    }

    /// Cut both logs back to the checkpoint's lengths, after checking both
    /// snapshots are the ones it recorded.
    fn cut_back(
        collection: &NativeCollection,
        medium: &dyn SegmentMedium,
        checkpoint: &SegmentCheckpointManifest,
    ) -> Result<bool, NativeStorageError> {
        let locations_match = crate::sha256_hex(&medium.read_location_snapshot()?)
            == checkpoint.locations_snapshot_sha256
            && medium.location_log_length()? >= checkpoint.locations_wal_length;
        if !locations_match {
            return Ok(false);
        }
        if medium.keeps_index() {
            // Nothing to cut: the index past its snapshot is the records'.
            return Ok(medium.read_index_snapshot()?.is_some_and(|snapshot| {
                crate::sha256_hex(&snapshot) == checkpoint.snapshot_sha256
            }));
        }
        let keys = collection.path.join("index");
        let keys_wal = keys.join("keys.wal");
        let keys_match = SegmentCheckpoint::digest(&keys.join("keys.snapshot"))?
            == checkpoint.snapshot_sha256
            && SegmentCheckpoint::file_length(&keys_wal)? >= checkpoint.keys_wal_length;
        if !keys_match {
            return Ok(false);
        }
        medium.truncate_location_log(checkpoint.locations_wal_length)?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&keys_wal)
            .and_then(|file| file.set_len(checkpoint.keys_wal_length))
            .map_err(NativeStorageError::io)?;
        Ok(true)
    }

    fn replay_commit(
        collection: &NativeCollection,
        (number, offset, length): (u64, u64, u32),
        record: &fylo_format::LogRecord,
    ) -> Result<Vec<SegmentLocation>, NativeStorageError> {
        let manifest = SegmentCommitManifest::decode(&record.manifest)?;
        let removed: BTreeSet<String> = manifest.index_removed.iter().cloned().collect();
        let added: BTreeSet<String> = manifest.index_added.iter().cloned().collect();
        NativeWriteRoot::append_index(collection, &NativeWriteRoot::index_lines(&removed, &added))?;
        SegmentRebuild::rows_of(number, (offset, length), record)
    }

    /// Point every location whose newest entry the compacted segment holds a
    /// copy of at that copy, and remove the segments it replaced.
    fn replay_compaction(
        medium: &dyn SegmentMedium,
        table: &LocationTable,
        record: &fylo_format::LogRecord,
    ) -> Result<Vec<SegmentLocation>, NativeStorageError> {
        let manifest = CompactionManifest::decode(&record.manifest)?;
        let rows = CompactionManifest::adopted_rows(medium, table, manifest.segment)?;
        for replaced in &manifest.replaces {
            medium.remove_segment(*replaced)?;
        }
        Ok(rows)
    }

    /// Remove every segment other than the active one that no location row
    /// refers to.
    pub(crate) fn remove_unreferenced(
        medium: &dyn SegmentMedium,
        active: u64,
    ) -> Result<(), NativeStorageError> {
        let table = LocationTable::load(medium, TableReach::written(medium)?)?;
        let referenced: BTreeSet<u64> = table.rows().iter().map(|row| row.segment).collect();
        for number in medium.segment_numbers()? {
            if number != active && !referenced.contains(&number) {
                medium.remove_segment(number)?;
            }
        }
        Ok(())
    }
}
