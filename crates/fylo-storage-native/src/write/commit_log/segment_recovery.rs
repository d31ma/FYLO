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
    LocationTable, SegmentCheckpointManifest, SegmentCommitManifest, SegmentLocation, SegmentPaths,
    SegmentStore, TableCache,
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
        let paths = SegmentPaths::of(&collection.path);
        let (number, scan) = Self::active_scan(&paths, stored)?;
        Self::require_torn_tail_only(&paths, number, &scan)?;
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
        let segment = LogSegment::open_scanned(
            &paths.segment(number),
            (number, scan.valid_length),
            next_lsn,
        )?;
        let generation = SegmentCheckpoint::bump_epoch(
            collection,
            &segment,
            SegmentCheckpoint::next_stable(stored, seen),
        )?;
        if !Self::replay_from_checkpoint(collection, &paths, (number, &scan))? {
            SegmentRebuild::run(collection)?;
        }
        TableCache::shared().forget(&paths);
        Self::remove_unreferenced(&paths, number)?;
        SegmentCheckpoint::run(collection, segment, generation)?;
        Ok(())
    }

    /// The active segment and its valid records. It is the segment the
    /// newest slot names, or else the highest-numbered segment a commit
    /// (not a compaction) wrote.
    fn active_scan(
        paths: &SegmentPaths,
        stored: &GenerationState,
    ) -> Result<(u64, LogScan), NativeStorageError> {
        let named = stored
            .log
            .as_ref()
            .and_then(|position| u64::from_str_radix(&position.nonce, 16).ok());
        let mut candidates: Vec<u64> = paths.segment_numbers()?;
        candidates.reverse();
        if let Some(named) = named {
            candidates.insert(0, named);
        }
        for number in candidates {
            if let Some(scan) = LogSegment::scan(&paths.segment(number))?
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
        paths: &SegmentPaths,
        number: u64,
        scan: &LogScan,
    ) -> Result<(), NativeStorageError> {
        let bytes = fylo_vfs::read(paths.segment(number)).map_err(NativeStorageError::io)?;
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
        paths: &SegmentPaths,
        (number, scan): (u64, &LogScan),
    ) -> Result<bool, NativeStorageError> {
        let Some((start, checkpoint)) = Self::last_checkpoint(scan) else {
            return Ok(false);
        };
        if !Self::cut_back(collection, paths, &checkpoint)? {
            return Ok(false);
        }
        let aborted = SegmentRebuild::aborted([scan])?;
        let mut table = LocationTable::load(paths, checkpoint.locations_wal_length)?;
        let lengths = SegmentRebuild::frame_lengths(scan);
        for (index, (offset, record)) in scan.records.iter().enumerate().skip(start + 1) {
            let rows = match record.kind {
                LogRecordKind::Commit if !aborted.contains(&record.lsn) => {
                    Self::replay_commit(collection, (number, *offset, lengths[index]), record)?
                }
                LogRecordKind::Compaction if !aborted.contains(&record.lsn) => {
                    Self::replay_compaction(paths, &table, record)?
                }
                _ => Vec::new(),
            };
            SegmentPlan::append_locations(&collection.path, &rows)?;
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
        paths: &SegmentPaths,
        checkpoint: &SegmentCheckpointManifest,
    ) -> Result<bool, NativeStorageError> {
        let keys = collection.path.join("index");
        let logs = [
            (
                paths.locations_snapshot(),
                &checkpoint.locations_snapshot_sha256,
                paths.locations_wal(),
                checkpoint.locations_wal_length,
            ),
            (
                keys.join("keys.snapshot"),
                &checkpoint.snapshot_sha256,
                keys.join("keys.wal"),
                checkpoint.keys_wal_length,
            ),
        ];
        for (snapshot, digest, wal, length) in &logs {
            let matches = SegmentCheckpoint::digest(snapshot)? == **digest
                && SegmentStore::file_length(wal)? >= *length;
            if !matches {
                return Ok(false);
            }
        }
        for (_, _, wal, length) in logs {
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&wal)
                .and_then(|file| file.set_len(length))
                .map_err(NativeStorageError::io)?;
        }
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
        paths: &SegmentPaths,
        table: &LocationTable,
        record: &fylo_format::LogRecord,
    ) -> Result<Vec<SegmentLocation>, NativeStorageError> {
        let manifest = CompactionManifest::decode(&record.manifest)?;
        let rows = CompactionManifest::adopted_rows(paths, table, manifest.segment)?;
        for replaced in &manifest.replaces {
            Self::remove_segment(paths, *replaced)?;
        }
        Ok(rows)
    }

    /// Remove every segment other than the active one that no location row
    /// refers to.
    pub(crate) fn remove_unreferenced(
        paths: &SegmentPaths,
        active: u64,
    ) -> Result<(), NativeStorageError> {
        let wal_length = SegmentStore::file_length(&paths.locations_wal())?;
        let table = LocationTable::load(paths, wal_length)?;
        let referenced: BTreeSet<u64> = table.rows().iter().map(|row| row.segment).collect();
        for number in paths.segment_numbers()? {
            if number != active && !referenced.contains(&number) {
                Self::remove_segment(paths, number)?;
            }
        }
        Ok(())
    }

    /// Remove a segment. On Windows another process may still hold it open;
    /// it is then left for the next recovery or compaction to remove.
    fn remove_segment(paths: &SegmentPaths, number: u64) -> Result<(), NativeStorageError> {
        match fylo_vfs::remove_file(paths.segment(number)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) if cfg!(windows) && error.kind() == std::io::ErrorKind::PermissionDenied => {
                Ok(())
            }
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }
}
