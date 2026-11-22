use std::collections::BTreeSet;
use std::path::Path;

use fylo_format::{LogRecordKind, LogScan};

use super::super::NativeWriteRoot;
use super::abort_manifest::AbortManifest;
use super::log_recovery::LogRecovery;
use super::namespace_log::NamespaceLog;
use super::namespace_manifest::NamespaceManifest;
use super::namespace_mark::NamespaceMark;
use super::namespace_part::NamespacePart;
use crate::{NativeStorageError, NativeStorageErrorCode};

/// Reads the namespace log for recovery: which parts a collection still
/// lacks, and which collections a crash may have left without theirs.
///
/// A part is applied by its collection's recovery when its generation is
/// above everything durable in that collection: its own log's records, the
/// generation its log segment opened at, a v1 state, and the checkpoint
/// marks the namespace log holds for it. A part that is missing from the
/// collection's log can only follow every record in it, because any later
/// flush of that log would have made the part durable too.
pub(crate) struct NamespaceRecovery;

impl NamespaceRecovery {
    /// The parts of `collection` above `covered`, in commit order, each with
    /// its own blobs.
    pub(crate) fn missing_parts(
        root: &Path,
        collection: &str,
        covered: u64,
    ) -> Result<Vec<(NamespacePart, Vec<u8>)>, NativeStorageError> {
        let Some(scan) = NamespaceLog::scan(root)? else {
            return Ok(Vec::new());
        };
        let covered = covered.max(Self::marked(&scan, collection)?);
        let aborted = Self::aborted(&scan)?;
        let mut missing = Vec::new();
        for (_, record) in &scan.records {
            if record.kind != LogRecordKind::Commit || aborted.contains(&record.lsn) {
                continue;
            }
            for part in Self::manifest(&record.manifest)?.parts {
                if part.collection == collection && part.generation > covered {
                    let blobs = part
                        .blobs(&record.blobs)
                        .ok_or_else(|| Self::corrupt("a part's blobs lie outside its record"))?
                        .to_vec();
                    missing.push((part, blobs));
                }
            }
        }
        Ok(missing)
    }

    /// Every collection a committed record of the namespace log names.
    pub(crate) fn collections(scan: &LogScan) -> Result<BTreeSet<String>, NativeStorageError> {
        let mut names = BTreeSet::new();
        for (_, record) in &scan.records {
            if record.kind == LogRecordKind::Commit {
                for part in Self::manifest(&record.manifest)?.parts {
                    names.insert(part.collection);
                }
            }
        }
        Ok(names)
    }

    /// The highest LSN of a commit record in `scan`.
    pub(crate) fn last_commit(scan: &LogScan) -> u64 {
        scan.records
            .iter()
            .filter(|(_, record)| record.kind == LogRecordKind::Commit)
            .map(|(_, record)| record.lsn)
            .max()
            .unwrap_or(0)
    }

    /// Whether `collection`'s parts need a namespace log at all: true when
    /// any committed record names it.
    pub(crate) fn names(root: &Path, collection: &str) -> Result<bool, NativeStorageError> {
        let Some(scan) = NamespaceLog::scan(root)? else {
            return Ok(false);
        };
        Ok(Self::collections(&scan)?.contains(collection))
    }

    fn marked(scan: &LogScan, collection: &str) -> Result<u64, NativeStorageError> {
        let mut generation = 0;
        for (_, record) in &scan.records {
            if record.kind != LogRecordKind::Checkpoint {
                continue;
            }
            let mark: NamespaceMark = serde_json::from_slice(&record.manifest)
                .map_err(|error| Self::corrupt(&error.to_string()))?;
            if mark.collection.as_deref() == Some(collection) {
                generation = generation.max(mark.generation);
            }
        }
        Ok(generation)
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

    fn manifest(bytes: &[u8]) -> Result<NamespaceManifest, NativeStorageError> {
        let manifest: NamespaceManifest =
            serde_json::from_slice(bytes).map_err(|error| Self::corrupt(&error.to_string()))?;
        if manifest.format != NamespaceManifest::FORMAT {
            return Err(Self::corrupt("unknown namespace commit format"));
        }
        Ok(manifest)
    }

    fn corrupt(detail: &str) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::CorruptMetadata,
            format!("namespace log record is corrupt: {detail}"),
        )
    }
}

impl NativeWriteRoot {
    /// Settle the namespace log a crash or an earlier process left: recover
    /// every collection it names, so each holds its parts durably, then
    /// remove the log. Returns whether there was a log to settle.
    ///
    /// # Errors
    ///
    /// Returns the first lock, recovery or removal failure; the log stays
    /// for the next attempt.
    pub fn recover_namespace(&self) -> Result<bool, NativeStorageError> {
        self.root.assert_profile_owned()?;
        if self.root.database().is_some() || !NamespaceLog::exists(self.path())? {
            return Ok(false);
        }
        let scan = NamespaceLog::scan(self.path())?;
        let names = scan
            .as_ref()
            .map_or_else(|| Ok(BTreeSet::new()), NamespaceRecovery::collections)?;
        let mut guards = Vec::with_capacity(names.len());
        for name in &names {
            let Some(collection) = self.collection_if_present(name)? else {
                continue;
            };
            guards.push(self.lock_collection_unsettled(&collection)?);
            self.settle_with_parts(&collection)?;
        }
        let last_commit = scan.as_ref().map_or(0, NamespaceRecovery::last_commit);
        NamespaceLog::shared(self.path())
            .remove_unless_committed_after(self.path(), last_commit)?;
        Ok(true)
    }

    /// Settle this namespace's log once per process, before the first
    /// write takes a collection lock. A `.vdbx` database keeps none.
    pub(crate) fn settle_namespace_once(&self) -> Result<(), NativeStorageError> {
        NamespaceLog::shared(self.path()).settle_once(|| self.recover_namespace())
    }

    /// Recover a collection when the namespace log holds parts it lacks,
    /// otherwise only checkpoint its log, so its files hold every part.
    fn settle_with_parts(
        &self,
        collection: &crate::NativeCollection,
    ) -> Result<(), NativeStorageError> {
        let stored = collection.stored_generation()?;
        let covered = LogRecovery::covered(collection, &stored)?;
        let lacking = NamespaceRecovery::missing_parts(self.path(), collection.name(), covered)?;
        if lacking.is_empty() {
            return self.settle_log(collection).map(drop);
        }
        LogRecovery::run(self, collection, &stored)
    }

    /// Settle the namespace log when it names `collection`, so an explicit
    /// recovery (`fylo recover`) of that collection always applies its parts,
    /// even in a process that settled the namespace before.
    pub(crate) fn recover_namespace_naming(
        &self,
        collection: &str,
    ) -> Result<bool, NativeStorageError> {
        if self.root.database().is_some() || !NamespaceRecovery::names(self.path(), collection)? {
            return Ok(false);
        }
        self.recover_namespace()
    }
}
