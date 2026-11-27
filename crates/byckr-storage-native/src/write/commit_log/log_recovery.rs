use std::collections::BTreeSet;

use byckr_format::{LogRecordKind, LogScan};
use byckr_vfs::OpenOptions;

use super::super::{GenerationRecord, NativeWriteRoot, write_json_durable};
use super::abort_manifest::AbortManifest;
use super::checkpoint_manifest::CheckpointManifest;
use super::commit_manifest::CommitManifest;
use super::log_checkpoint::LogCheckpoint;
use super::log_segment::LogSegment;
use super::namespace_part::NamespacePart;
use super::namespace_recovery::NamespaceRecovery;
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
        let (replayed, index_trusted, log_valid) = Self::replay_own_log(writer, collection)?;
        let covered = Self::covered(collection, stored)?;
        let parts = NamespaceRecovery::missing_parts(writer.path(), collection.name(), covered)?;
        let applied = Self::apply_parts(collection, writer, &parts, index_trusted && log_valid)?;
        if !index_trusted || (!parts.is_empty() && !log_valid) {
            writer.rebuild_index(collection)?;
        }
        let generation = stored.generation.max(replayed).max(applied);
        let stable = generation + 2 - generation % 2;
        LogCheckpoint::run(collection, stable)?;
        let state = LogCheckpoint::state_path(collection)?;
        let fenced = collection.generation_fenced(stored);
        write_json_durable(&state, &GenerationRecord::stable(stable).fenced(fenced))
    }

    /// Replay the collection's own log. Returns the highest generation it
    /// replayed, whether the index log can be trusted, and whether the log
    /// had a valid opening checkpoint.
    fn replay_own_log(
        writer: &NativeWriteRoot,
        collection: &NativeCollection,
    ) -> Result<(u64, bool, bool), NativeStorageError> {
        let scan = LogSegment::scan(&collection.log_segment_path())?;
        let checkpoint = scan.as_ref().and_then(Self::opening_checkpoint);
        if let (Some(scan), Some(checkpoint)) = (&scan, checkpoint) {
            let (replayed, trusted) = Self::replay(writer, collection, scan, &checkpoint)?;
            return Ok((replayed, trusted, true));
        }
        Self::require_untouched_index(writer, collection, scan.as_ref())?;
        Ok((0, true, false))
    }

    /// The highest generation durable in the collection's own files and
    /// log: a v1 state, the generation its segment opened at, and its
    /// commit records.
    pub(crate) fn covered(
        collection: &NativeCollection,
        stored: &GenerationState,
    ) -> Result<u64, NativeStorageError> {
        let durable_state = if stored.log.is_none() {
            stored.generation
        } else {
            0
        };
        let Some(scan) = LogSegment::scan(&collection.log_segment_path())? else {
            return Ok(durable_state);
        };
        let records = scan
            .records
            .iter()
            .filter(|(_, record)| record.kind == LogRecordKind::Commit)
            .map(|(_, record)| record.generation)
            .max()
            .unwrap_or(0);
        Ok(durable_state.max(Self::opened_at(&scan)).max(records))
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
    ) -> Result<(u64, bool), NativeStorageError> {
        let index_trusted = Self::cut_index_back(collection, checkpoint)?;
        let aborted = Self::aborted(scan)?;
        let applier = StepApplier::replay(&writer.root, &collection.path);
        let mut generation = 0;
        for (_, record) in &scan.records {
            // Compaction and schema records belong to document segments
            // only; one in a write-ahead log is damage, not something to
            // skip.
            if matches!(
                record.kind,
                LogRecordKind::Compaction | LogRecordKind::Schema
            ) {
                return Err(Self::corrupt("write-ahead log holds a segment-only record"));
            }
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
        Ok((generation, index_trusted))
    }

    /// The generation a segment was opened at: everything at or below it
    /// was checkpointed into the collection's files.
    fn opened_at(scan: &LogScan) -> u64 {
        scan.records
            .first()
            .map_or(0, |(_, record)| record.generation)
    }

    /// Apply the namespace parts this collection's log lost, after every
    /// record of the log. Returns the highest generation they publish.
    fn apply_parts(
        collection: &NativeCollection,
        writer: &NativeWriteRoot,
        parts: &[(NamespacePart, Vec<u8>)],
        append_index: bool,
    ) -> Result<u64, NativeStorageError> {
        let applier = StepApplier::replay(&writer.root, &collection.path);
        let mut generation = 0;
        for (part, blobs) in parts {
            for step in &part.commit.steps {
                applier.apply(step, blobs)?;
            }
            if append_index {
                Self::append_delta(collection, &part.commit)?;
            }
            generation = generation.max(part.generation);
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
