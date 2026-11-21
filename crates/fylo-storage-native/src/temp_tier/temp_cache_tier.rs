use std::io::{self, Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::cache_sweep::CacheSweep;
use super::document_entry::DocumentEntry;
use super::entry_key::EntryKey;
use super::index_entry::IndexEntry;
use super::private_folder::PrivateFolder;
use super::sweep_report::SweepReport;
use super::temp_cache_mode::TempCacheMode;
use super::temp_cache_status::TempCacheStatus;
use super::tier_counters::TierCounters;
use super::tier_failpoint::TierFailpoint;

/// An active temp-disk read tier (ADR 0030 section 4), shared by every root
/// a session opens.
///
/// It reads and writes entries below one private cache folder. It never
/// decides whether an entry is current: the collection that asks compares
/// the stamps. A problem with an entry is counted and answered as a miss,
/// so the tier can never fail a read (section 8).
#[derive(Debug)]
pub struct TempCacheTier {
    folder: PrivateFolder,
    mode: TempCacheMode,
    budget: u64,
    counters: TierCounters,
    last_sweep: Mutex<SweepReport>,
    written_since_sweep: AtomicU64,
    writes_paused: AtomicBool,
    serial: AtomicU64,
}

impl TempCacheTier {
    const LARGEST_SHARE: u64 = 64;
    const SWEEP_SHARE: u64 = 10;
    const ENTRY_OVERHEAD: u64 = 3 * 65_536 + 128;
    const RECENT: Duration = Duration::from_mins(1);

    pub(crate) fn new(
        folder: PrivateFolder,
        mode: TempCacheMode,
        budget: u64,
        sweep: SweepReport,
    ) -> Self {
        Self {
            folder,
            mode,
            budget,
            counters: TierCounters::default(),
            last_sweep: Mutex::new(sweep),
            written_since_sweep: AtomicU64::new(0),
            writes_paused: AtomicBool::new(false),
            serial: AtomicU64::new(0),
        }
    }

    /// The tier as the handshake reports it, with this process's counters.
    #[must_use]
    pub fn status(&self) -> TempCacheStatus {
        let counters = &self.counters;
        let sweep = self.sweep_report();
        TempCacheStatus {
            mode: self.mode.as_str(),
            active: true,
            reason: None,
            dir: self.folder.path().display().to_string(),
            budget_bytes: self.budget,
            used_bytes: sweep.used_bytes,
            entries: sweep.entries,
            hits: TierCounters::read(&counters.hits),
            misses: TierCounters::read(&counters.misses),
            revalidations: TierCounters::read(&counters.revalidations),
            rejected: TierCounters::read(&counters.rejected),
            writes: TierCounters::read(&counters.writes),
            evictions: TierCounters::read(&counters.evictions),
            errors: TierCounters::read(&counters.errors),
        }
    }

    /// The entry at `key`, if one is there and it is a sound entry of
    /// `identifier` in `collection`. A damaged or foreign entry is deleted
    /// and counted as rejected.
    pub(crate) fn read(
        &self,
        key: &EntryKey,
        collection: &str,
        identifier: &str,
    ) -> Option<DocumentEntry> {
        let bytes = self.read_bytes(key)?;
        let entry = DocumentEntry::decode(&bytes)
            .filter(|entry| entry.collection == collection && entry.identifier == identifier);
        self.reject_unless_found(key, entry)
    }

    /// The index entry at `key`, if one is there and it is a sound entry of
    /// `collection`. A damaged or foreign entry is deleted and counted as
    /// rejected.
    pub(crate) fn read_index(&self, key: &EntryKey, collection: &str) -> Option<IndexEntry> {
        let bytes = self.read_bytes(key)?;
        let entry = IndexEntry::decode(&bytes).filter(|entry| entry.collection == collection);
        self.reject_unless_found(key, entry)
    }

    /// Whether a stored payload of `bytes` may be written: one entry never
    /// takes more than 1/64 of the budget.
    pub(crate) fn admits(&self, bytes: usize) -> bool {
        u64::try_from(bytes).is_ok_and(|bytes| bytes <= self.budget / Self::LARGEST_SHARE)
    }

    /// The payload bytes a collection handle may hold back for one read
    /// attempt before writing them: a tenth of the budget.
    pub(crate) fn pending_limit(&self) -> usize {
        usize::try_from(self.budget / Self::SWEEP_SHARE).unwrap_or(usize::MAX)
    }

    /// Write the encoded entry `bytes` at `key`: a partial file of this
    /// process first, then a rename over the final name, so a reader never
    /// sees half an entry. A failed write is counted and pauses writing
    /// until the next sweep.
    pub(crate) fn write(&self, key: &EntryKey, bytes: &[u8]) {
        if self.writes_paused.load(Ordering::Relaxed) {
            self.sweep(SystemTime::now());
            return;
        }
        match self.write_bytes(key, bytes) {
            Ok(()) => self.written(bytes.len()),
            Err(_) => self.write_failed(),
        }
    }

    /// Delete the entry at `key`, counted as rejected: it was damaged, or it
    /// names a document file that has changed or gone.
    pub(crate) fn reject(&self, key: &EntryKey) {
        TierCounters::count(&self.counters.rejected);
        let _ = self.folder.remove_file(&key.path());
    }

    /// Delete the entry at `key` without counting it: it is sound but can
    /// no longer be used, such as an index entry of an older stamp.
    pub(crate) fn discard(&self, key: &EntryKey) {
        let _ = self.folder.remove_file(&key.path());
    }

    /// Count a read the tier answered.
    pub(crate) fn count_hit(&self) {
        TierCounters::count(&self.counters.hits);
    }

    /// Count a read the tier could not answer.
    pub(crate) fn count_miss(&self) {
        TierCounters::count(&self.counters.misses);
    }

    /// Count an entry checked again against the root's file.
    pub(crate) fn count_revalidation(&self) {
        TierCounters::count(&self.counters.revalidations);
    }

    fn read_bytes(&self, key: &EntryKey) -> Option<Vec<u8>> {
        let opened = self.open_entry(key);
        if opened.is_err() {
            TierCounters::count(&self.counters.errors);
        }
        opened.ok().flatten()
    }

    fn open_entry(&self, key: &EntryKey) -> io::Result<Option<Vec<u8>>> {
        let file = match self.folder.open_file(&key.path()) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => other?,
        };
        let limit = self.read_limit();
        if file.metadata()?.len() > limit {
            return Ok(None);
        }
        Self::refresh_recency(&file);
        let mut bytes = Vec::new();
        file.take(limit).read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }

    /// The largest entry file this process reads: one entry of its own
    /// budget, or one document, whichever is larger. A larger file, kept by
    /// a process with a larger budget, is a miss and is left alone.
    fn read_limit(&self) -> u64 {
        let share = self.budget / Self::LARGEST_SHARE;
        share.max(crate::MAX_DOCUMENT_BYTES) + Self::ENTRY_OVERHEAD
    }

    fn refresh_recency(file: &std::fs::File) {
        let now = SystemTime::now();
        let modified = file.metadata().and_then(|metadata| metadata.modified());
        let stale = modified.is_ok_and(|modified| {
            now.duration_since(modified)
                .is_ok_and(|age| age > Self::RECENT)
        });
        if stale {
            let _ = file.set_modified(now);
        }
    }

    fn reject_unless_found<T>(&self, key: &EntryKey, entry: Option<T>) -> Option<T> {
        if entry.is_none() {
            self.reject(key);
        }
        entry
    }

    fn write_bytes(&self, key: &EntryKey, bytes: &[u8]) -> io::Result<()> {
        self.folder.create_folders(key.folder())?;
        let partial = key.partial_name(&self.partial_suffix());
        let result = self.write_partial(key, &partial, bytes);
        if result.is_err() {
            let _ = self.folder.remove_file(&key.folder().join(&partial));
        }
        result
    }

    /// A suffix no other process, nor an earlier process with the same pid,
    /// is likely to use: the time in nanoseconds and a serial of this tier.
    fn partial_suffix(&self) -> String {
        let nanoseconds = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        format!("{nanoseconds:x}-{serial}")
    }

    fn write_partial(&self, key: &EntryKey, partial: &str, bytes: &[u8]) -> io::Result<()> {
        let mut file = self.folder.create_file(&key.folder().join(partial))?;
        let (first, second) = bytes.split_at(bytes.len() / 2);
        file.write_all(first)?;
        TierFailpoint::reach(TierFailpoint::TORN_PARTIAL);
        file.write_all(second)?;
        drop(file);
        TierFailpoint::reach(TierFailpoint::BEFORE_RENAME);
        self.folder.rename_file(key.folder(), partial, key.name())
    }

    fn written(&self, bytes: usize) {
        TierCounters::count(&self.counters.writes);
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        let total = self.written_since_sweep.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if total > self.budget / Self::SWEEP_SHARE {
            self.sweep(SystemTime::now());
        }
    }

    fn write_failed(&self) {
        TierCounters::count(&self.counters.errors);
        self.writes_paused.store(true, Ordering::Relaxed);
    }

    fn sweep(&self, now: SystemTime) {
        let sweep = CacheSweep::new(self.budget, now);
        if !sweep.is_due(&self.folder) {
            return;
        }
        let report = sweep.run(&self.folder).unwrap_or_default();
        TierCounters::add(&self.counters.evictions, report.evicted);
        self.written_since_sweep.store(0, Ordering::Relaxed);
        self.writes_paused.store(false, Ordering::Relaxed);
        *self
            .last_sweep
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = report;
    }

    fn sweep_report(&self) -> SweepReport {
        *self
            .last_sweep
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
