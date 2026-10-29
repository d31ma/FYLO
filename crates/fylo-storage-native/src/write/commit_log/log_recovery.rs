use std::collections::BTreeSet;

use fylo_format::{LogRecordKind, LogScan};
use fylo_vfs::OpenOptions;

use super::super::{GenerationRecord, NativeWriteRoot, write_json_durable};
use super::abort_manifest::AbortManifest;
use super::checkpoint_manifest::CheckpointManifest;
use super::commit_manifest::CommitManifest;
use super::log_checkpoint::LogCheckpoint;
use super::log_segment::LogSegment;
use super::step_applier::StepApplier;
use crate::{
    GenerationState, NativeCollection, NativeStorageError, NativeStorageErrorCode,
    path_exists_no_follow,
};

/// Replays a collection's write-ahead log after a crash.
///
/// Recovery is redo-only. It cuts the index log back to the state the
/// segment's checkpoint recorded, replays every commit no abort cancelled in
/// order, rebuilds the index from documents when its recorded state cannot
/// be trusted, then checkpoints the log away.
pub(crate) struct LogRecovery;

impl LogRecovery {
    pub(crate) fn run(
        writer: &NativeWriteRoot,
        collection: &NativeCollection,
        stored: &GenerationState,
    ) -> Result<(), NativeStorageError> {
        let scan = LogSegment::scan(&collection.log_segment_path())?;
        let checkpoint = scan.as_ref().and_then(Self::opening_checkpoint);
        let mut generation = stored.generation;
        match (&scan, checkpoint) {
            (Some(scan), Some(checkpoint)) => {
                generation = generation.max(Self::replay(writer, collection, scan, &checkpoint)?);
            }
            _ => Self::require_untouched_index(writer, collection, scan.as_ref())?,
        }
        let stable = generation + 2 - generation % 2;
        LogCheckpoint::run(collection, stable)?;
        let state = LogCheckpoint::state_path(collection)?;
        let fenced = collection.generation_fenced(stored);
        write_json_durable(&state, &GenerationRecord::stable(stable).fenced(fenced))
    }

    /// The checkpoint record every segment opens with, if the segment got
    /// far enough to hold one.
    fn opening_checkpoint(scan: &LogScan) -> Option<CheckpointManifest> {
        let (_, first) = scan.records.first()?;
        if first.kind != LogRecordKind::Checkpoint {
            return None;
        }
        serde_json::from_slice::<CheckpointManifest>(&first.manifest)
            .ok()
            .filter(|manifest| manifest.format == CheckpointManifest::FORMAT)
    }

    /// A segment that never got its opening checkpoint durable carried no
    /// commit, because nothing is applied before its first flush. Only the
    /// index could disagree, and only if something else changed it, so it is
    /// rebuilt to be sure.
    fn require_untouched_index(
        writer: &NativeWriteRoot,
        collection: &NativeCollection,
        scan: Option<&LogScan>,
    ) -> Result<(), NativeStorageError> {
        if scan.is_some_and(|scan| scan.records.len() > 1) {
            writer.rebuild_index(collection)?;
        }
        Ok(())
    }

    fn replay(
        writer: &NativeWriteRoot,
        collection: &NativeCollection,
        scan: &LogScan,
        checkpoint: &CheckpointManifest,
    ) -> Result<u64, NativeStorageError> {
        let index_trusted = Self::cut_index_back(collection, checkpoint)?;
        let aborted = Self::aborted(scan)?;
        let applier = StepApplier::replay(&writer.root, &collection.path);
        let mut generation = 0;
        for (_, record) in &scan.records {
            if record.kind != LogRecordKind::Commit || aborted.contains(&record.lsn) {
                continue;
            }
            let manifest = Self::commit_manifest(&record.manifest)?;
            for step in &manifest.steps {
                applier.apply(step, &record.blobs)?;
            }
            if index_trusted {
                Self::append_delta(collection, &manifest)?;
            }
            generation = generation.max(record.generation);
        }
        if !index_trusted {
            writer.rebuild_index(collection)?;
        }
        Ok(generation)
    }

    /// Cut `keys.wal` back to its length at the checkpoint. Returns false
    /// when the index no longer matches what the checkpoint recorded.
    fn cut_index_back(
        collection: &NativeCollection,
        checkpoint: &CheckpointManifest,
    ) -> Result<bool, NativeStorageError> {
        let index = collection.path.join("index");
        let wal = index.join("keys.wal");
        let snapshot = std::fs::read(index.join("keys.snapshot")).unwrap_or_default();
        if crate::sha256_hex(&snapshot) != checkpoint.snapshot_sha256 {
            return Ok(false);
        }
        if LogCheckpoint::length(&wal)? < checkpoint.keys_wal_length {
            return Ok(false);
        }
        if path_exists_no_follow(&wal)? {
            OpenOptions::new()
                .write(true)
                .open(&wal)
                .and_then(|file| file.set_len(checkpoint.keys_wal_length))
                .map_err(NativeStorageError::io)?;
        }
        Ok(true)
    }

    fn aborted(scan: &LogScan) -> Result<BTreeSet<u64>, NativeStorageError> {
        scan.records
            .iter()
            .filter(|(_, record)| record.kind == LogRecordKind::Abort)
            .map(|(_, record)| {
                serde_json::from_slice::<AbortManifest>(&record.manifest)
                    .map(|manifest| manifest.aborts)
                    .map_err(|error| Self::corrupt(&error.to_string()))
            })
            .collect()
    }

    fn commit_manifest(bytes: &[u8]) -> Result<CommitManifest, NativeStorageError> {
        let manifest: CommitManifest =
            serde_json::from_slice(bytes).map_err(|error| Self::corrupt(&error.to_string()))?;
        if manifest.format != CommitManifest::FORMAT {
            return Err(Self::corrupt("unknown commit format"));
        }
        Ok(manifest)
    }

    fn append_delta(
        collection: &NativeCollection,
        manifest: &CommitManifest,
    ) -> Result<(), NativeStorageError> {
        let mut lines = Vec::new();
        for (sign, key) in manifest
            .index_removed
            .iter()
            .map(|key| (b'-', key))
            .chain(manifest.index_added.iter().map(|key| (b'+', key)))
        {
            lines.push(sign);
            lines.push(b'\t');
            lines.extend_from_slice(key.as_bytes());
            lines.push(b'\n');
        }
        NativeWriteRoot::append_index(collection, &lines)
    }

    fn corrupt(detail: &str) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::CorruptMetadata,
            format!("write-ahead log record is corrupt: {detail}"),
        )
    }
}
