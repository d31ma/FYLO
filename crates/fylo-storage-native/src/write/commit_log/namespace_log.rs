use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use fylo_format::{LogRecord, LogRecordKind, LogScan};
use fylo_vfs as fs;

use super::super::{ensure_directory, failpoint, json_error, remove_dir_durable};
use super::log_segment::LogSegment;
use super::namespace_entry::NamespaceEntry;
use super::namespace_mark::NamespaceMark;
use super::namespace_recovery::NamespaceRecovery;
use crate::fresh_read::FreshRead;
use crate::{Durability, NativeRoot, NativeStorageError, path_exists_no_follow};

/// The namespace's own write-ahead log, where a transaction over several
/// collections commits as one record with one flush.
///
/// One per namespace root in a process. Appends hold its mutex through the
/// flush, and it is always the last lock a writer takes, after every
/// collection lock, so it cannot take part in a deadlock.
#[derive(Debug, Default)]
pub(crate) struct NamespaceLog {
    segment: Mutex<Option<LogSegment>>,
    /// Held while this process settles a namespace log an earlier process
    /// left; `true` once that is done.
    settled: Mutex<bool>,
}

impl NamespaceLog {
    const TRANSACTIONS: &'static str = ".fylo-transactions";
    const FOLDER: &'static str = ".namespace";
    const CHECKPOINT_RECORDS: u64 = 4_096;
    const CHECKPOINT_BYTES: u64 = 16 * 1024 * 1024;
    /// Recovery and settling read the namespace log whole, so the read goes
    /// through the one shared fresh-read helper (ADR 0013, Amendment 3, and
    /// ADR 0033 section 4): an open and a read, never cached attributes, and
    /// `F_NOCACHE` on macOS. The log checkpoints at `CHECKPOINT_BYTES`, but a
    /// single commit record may push one segment past it before settling, so
    /// the bound matches a file-based log segment's own ceiling.
    const SCAN_LIMIT: u64 = 1024 * 1024 * 1024;

    /// The namespace log of the namespace root at `root`.
    pub(crate) fn shared(root: &Path) -> Arc<Self> {
        let mut logs = Self::registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Arc::clone(logs.entry(root.to_owned()).or_default())
    }

    fn registry() -> &'static Mutex<BTreeMap<PathBuf, Arc<Self>>> {
        static LOGS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<NamespaceLog>>>> = OnceLock::new();
        LOGS.get_or_init(Mutex::default)
    }

    pub(crate) fn segment_path(root: &Path) -> PathBuf {
        Self::directory(root).join("segment.log")
    }

    fn directory(root: &Path) -> PathBuf {
        root.join(Self::TRANSACTIONS).join(Self::FOLDER).join("log")
    }

    /// Whether a namespace log is on disk under `root`, decided by opening
    /// its segment: on a network root a `stat` may answer from cached
    /// attributes, an open does not.
    pub(crate) fn exists(root: &Path) -> Result<bool, NativeStorageError> {
        match fs::File::open(Self::segment_path(root)) {
            Ok(_) => Ok(true),
            Err(error) if Self::absent(&error) => Ok(false),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn absent(error: &std::io::Error) -> bool {
        matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        )
    }

    /// Whether the namespace log has grown enough to settle it, by the
    /// limits a collection log checkpoints at.
    pub(crate) fn wants_checkpoint(&self) -> bool {
        self.lock().as_ref().is_some_and(|segment| {
            segment.records() >= Self::CHECKPOINT_RECORDS
                || segment.length() >= Self::CHECKPOINT_BYTES
        })
    }

    /// Every valid record of the namespace log, or `None` without one.
    ///
    /// The segment is read through [`FreshRead`] so a network file system
    /// cannot answer from cached bytes: an open and a whole read, with the
    /// client's data cache turned off on macOS. A `None` means the file is
    /// absent or its header never became valid.
    pub(crate) fn scan(root: &Path) -> Result<Option<LogScan>, NativeStorageError> {
        match FreshRead::bytes(&Self::segment_path(root), Self::SCAN_LIMIT) {
            Ok(bytes) => Ok(LogScan::segment(&bytes).ok()),
            Err(error) if Self::absent(&error) => Ok(None),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    /// Append `entry` and flush it, after a fresh check that this process
    /// still holds the namespace lease. Returns the record's LSN once it is
    /// durable. A failed append or flush cuts the record back off, and the
    /// handle is never flushed again: the next append reopens the file.
    pub(crate) fn append_durably(
        &self,
        root: &NativeRoot,
        entry: NamespaceEntry<'_>,
        durability: Durability,
    ) -> Result<u64, NativeStorageError> {
        let mut segment = self.lock();
        root.check_profile_owned()?;
        let open = Self::open_segment(root, &mut segment)?;
        let before = open.length();
        let lsn = open.next_lsn();
        let outcome = Self::append_and_flush(open, lsn, entry, durability);
        if outcome.is_err() {
            let _ = open.truncate(before);
            *segment = None;
        }
        outcome.map(|()| lsn)
    }

    fn append_and_flush(
        segment: &mut LogSegment,
        lsn: u64,
        entry: NamespaceEntry<'_>,
        durability: Durability,
    ) -> Result<(), NativeStorageError> {
        let record = LogRecord {
            lsn,
            kind: entry.kind,
            generation: 0,
            manifest: entry.manifest,
            blobs: entry.blobs,
        };
        let encoded = segment.encode(&record)?;
        segment.append(&encoded, 1)?;
        if let Some((before, _)) = entry.failpoints {
            failpoint(before)?;
        }
        segment.flush_handle().run(durability)?;
        segment.mark_flushed();
        match entry.failpoints {
            Some((_, after)) => failpoint(after),
            None => Ok(()),
        }
    }

    fn open_segment<'a>(
        root: &NativeRoot,
        segment: &'a mut Option<LogSegment>,
    ) -> Result<&'a mut LogSegment, NativeStorageError> {
        if segment.is_none() {
            *segment = Some(Self::reopen_or_create(root)?);
        }
        Ok(segment.as_mut().expect("opened above"))
    }

    fn reopen_or_create(root: &NativeRoot) -> Result<LogSegment, NativeStorageError> {
        let path = Self::segment_path(root.path());
        if let Some(existing) = LogSegment::open_log_at_end(&path)? {
            return Ok(existing);
        }
        let folder = root.path().join(Self::TRANSACTIONS).join(Self::FOLDER);
        ensure_directory(root, &root.path().join(Self::TRANSACTIONS))?;
        ensure_directory(root, &folder)?;
        let opening =
            serde_json::to_vec(&NamespaceMark::opening()).map_err(|error| json_error(&error))?;
        LogSegment::create_opened_by(&path, opening, 0)
    }

    /// Close the handle and remove the namespace log durably, once every
    /// collection it names has its parts durable in its own files, unless a
    /// commit after `last_commit` arrived meanwhile.
    pub(crate) fn remove_unless_committed_after(
        &self,
        root: &Path,
        last_commit: u64,
    ) -> Result<(), NativeStorageError> {
        let mut segment = self.lock();
        let newer = Self::scan(root)?
            .is_some_and(|scan| NamespaceRecovery::last_commit(&scan) > last_commit);
        if newer {
            return Ok(());
        }
        *segment = None;
        let directory = Self::directory(root);
        if !path_exists_no_follow(&directory)? {
            return Ok(());
        }
        remove_dir_durable(&directory)
    }

    /// Record that `collection` checkpointed its own log at `generation`,
    /// before the checkpoint removes that log, when the namespace log holds
    /// parts of it. Recovery then never applies those parts again over
    /// later writes.
    pub(crate) fn mark_checkpoint(
        root: &NativeRoot,
        collection: &str,
        generation: u64,
    ) -> Result<(), NativeStorageError> {
        let none = root.database().is_some() || !Self::exists(root.path())?;
        if none || !NamespaceRecovery::names(root.path(), collection)? {
            return Ok(());
        }
        let mark = NamespaceMark::checkpointed(collection, generation);
        let entry = NamespaceEntry {
            kind: LogRecordKind::Checkpoint,
            manifest: serde_json::to_vec(&mark).map_err(|error| json_error(&error))?,
            blobs: Vec::new(),
            failpoints: None,
        };
        Self::shared(root.path())
            .append_durably(root, entry, Durability::PowerLoss)
            .map(drop)
    }

    /// Run `settle` once per process for this namespace, before its first
    /// write takes any collection lock. A failed attempt is tried again by
    /// the next write.
    pub(crate) fn settle_once(
        &self,
        settle: impl FnOnce() -> Result<bool, NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        let mut settled = self.settled.lock().unwrap_or_else(PoisonError::into_inner);
        if *settled {
            return Ok(());
        }
        settle()?;
        *settled = true;
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Option<LogSegment>> {
        self.segment.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
