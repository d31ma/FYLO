use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use fylo_format::{LogRecordKind, LogScan};
use fylo_vfs::{self as fs, File};

use super::super::{
    GenerationRecord, INDEX_WAL_COMPACT_BYTES, durable_replace, failpoint, remove_dir_durable,
    safe_relative_path, sync_directory, write_json_durable,
};
use super::commit_manifest::CommitManifest;
use super::commit_step::CommitStep;
use super::log_segment::LogSegment;
use crate::{NativeCollection, NativeStorageError, NativeStorageErrorCode, path_exists_no_follow};

/// Makes every applied commit durable in its files and removes the log.
///
/// Afterwards the collection is exactly the layout the journal and every
/// reader already know, and `state.json` is back to the v1 format, or to v3
/// for a collection whose schemas live in the registry.
pub(crate) struct LogCheckpoint;

impl LogCheckpoint {
    pub(crate) fn run(
        collection: &NativeCollection,
        generation: u64,
    ) -> Result<(), NativeStorageError> {
        let directory = collection.log_directory();
        if !path_exists_no_follow(&directory)? {
            return Ok(());
        }
        let touched = Self::touched_paths(collection)?;
        Self::flush_paths(&touched)?;
        Self::compact_index(collection)?;
        Self::full_flush(&collection.log_segment_path())?;
        failpoint("after-checkpoint-flush")?;
        let state = Self::state_path(collection)?;
        let fenced = collection.generation_fenced(&collection.stored_generation()?);
        write_json_durable(&state, &GenerationRecord::stable(generation).fenced(fenced))?;
        remove_dir_durable(&directory)?;
        failpoint("after-checkpoint-remove")
    }

    /// Every file and directory the segment's commits wrote, plus the index.
    pub(crate) fn touched_paths(
        collection: &NativeCollection,
    ) -> Result<BTreeSet<PathBuf>, NativeStorageError> {
        let mut paths = BTreeSet::new();
        let index = collection.path.join("index");
        paths.insert(index.join("keys.wal"));
        paths.insert(index.join("keys.snapshot"));
        let Some(scan) = LogSegment::scan(&collection.log_segment_path())? else {
            return Ok(paths);
        };
        for manifest in Self::commit_manifests(&scan)? {
            for step in &manifest.steps {
                Self::step_paths(collection, step, &mut paths)?;
            }
        }
        Ok(paths)
    }

    pub(crate) fn commit_manifests(
        scan: &LogScan,
    ) -> Result<Vec<CommitManifest>, NativeStorageError> {
        scan.records
            .iter()
            .filter(|(_, record)| record.kind == LogRecordKind::Commit)
            .map(|(_, record)| {
                serde_json::from_slice(&record.manifest).map_err(|error| {
                    NativeStorageError::new(
                        NativeStorageErrorCode::CorruptMetadata,
                        format!("write-ahead log commit manifest is corrupt: {error}"),
                    )
                })
            })
            .collect()
    }

    pub(crate) fn state_path(collection: &NativeCollection) -> Result<PathBuf, NativeStorageError> {
        collection
            .log_directory()
            .parent()
            .map(|parent| parent.join("state.json"))
            .ok_or_else(|| {
                NativeStorageError::new(NativeStorageErrorCode::UnsafePath, "log has no parent")
            })
    }

    fn step_paths(
        collection: &NativeCollection,
        step: &CommitStep,
        paths: &mut BTreeSet<PathBuf>,
    ) -> Result<(), NativeStorageError> {
        let relative = match step {
            CommitStep::Update(update) => vec![update.path.as_str()],
            CommitStep::Move(rename) => vec![rename.from.as_str(), rename.to.as_str()],
        };
        for value in relative {
            paths.insert(collection.path.join(safe_relative_path(value)?));
        }
        Ok(())
    }

    fn flush_paths(paths: &BTreeSet<PathBuf>) -> Result<(), NativeStorageError> {
        let mut directories = BTreeSet::new();
        for path in paths {
            if path_exists_no_follow(path)? {
                let file = Self::open_for_flush(path)?;
                crate::sync_handle(&file).map_err(NativeStorageError::io)?;
            }
            if let Some(parent) = path.parent() {
                directories.insert(parent.to_owned());
            }
        }
        for directory in directories {
            if path_exists_no_follow(&directory)? {
                sync_directory(&directory)?;
            }
        }
        Ok(())
    }

    pub(crate) fn compact_index(collection: &NativeCollection) -> Result<(), NativeStorageError> {
        if collection.index_in_medium()? {
            return Ok(());
        }
        let index = collection.path.join("index");
        let wal = index.join("keys.wal");
        let snapshot = index.join("keys.snapshot");
        if Self::length(&wal)? <= INDEX_WAL_COMPACT_BYTES.max(Self::length(&snapshot)?) {
            return Ok(());
        }
        let merged = collection.index_snapshot()?;
        durable_replace(&snapshot, merged.as_bytes())?;
        durable_replace(&wal, b"")
    }

    /// The index log has outgrown the size a reader should merge.
    pub(crate) fn index_wants_compaction(
        collection: &NativeCollection,
    ) -> Result<bool, NativeStorageError> {
        if collection.index_in_medium()? {
            return Ok(false);
        }
        let index = collection.path.join("index");
        let wal = Self::length(&index.join("keys.wal"))?;
        let snapshot = Self::length(&index.join("keys.snapshot"))?;
        Ok(wal > INDEX_WAL_COMPACT_BYTES.max(snapshot))
    }

    pub(crate) fn length(path: &Path) -> Result<u64, NativeStorageError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
            Ok(_) => Ok(0),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    /// One drive-cache flush, so a checkpoint survives power loss on Apple
    /// platforms too. Elsewhere `sync_all` is the same flush `fsync` is.
    fn full_flush(segment: &Path) -> Result<(), NativeStorageError> {
        if !path_exists_no_follow(segment)? {
            return Ok(());
        }
        Self::open_for_flush(segment)?
            .sync_all()
            .map_err(NativeStorageError::io)
    }

    /// Windows flushes only a handle opened for writing; POSIX flushes any
    /// handle, and a read-only record must not need write permission.
    #[cfg(windows)]
    fn open_for_flush(path: &Path) -> Result<File, NativeStorageError> {
        fylo_vfs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(NativeStorageError::io)
    }

    #[cfg(not(windows))]
    fn open_for_flush(path: &Path) -> Result<File, NativeStorageError> {
        File::open(path).map_err(NativeStorageError::io)
    }
}
