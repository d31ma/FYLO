use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::cache_file::CacheFile;
use super::cache_file_kind::CacheFileKind;
use super::cache_listing::CacheListing;
use super::private_folder::PrivateFolder;
use super::sweep_report::SweepReport;

/// Keeps a temp cache folder under its budget (ADR 0030 section 4.5).
///
/// A sweep lists the folder, removes partial files older than ten minutes,
/// removes the least recently used entries (oldest modification time
/// first) until the rest take at most 90% of the budget, and writes
/// `sweep.stamp`, whose modification time is the last sweep. Processes
/// sweep at most once per 30 seconds between them. It takes no lock: two
/// sweeps at once only remove a file twice.
#[derive(Clone, Copy, Debug)]
pub struct CacheSweep {
    budget: u64,
    now: SystemTime,
}

impl CacheSweep {
    /// The shortest time between two sweeps of one folder.
    pub const INTERVAL: Duration = Duration::from_secs(30);
    /// The age at which a partial file is left over from a crash.
    pub const PARTIAL_AGE: Duration = Duration::from_mins(10);
    const STAMP: &'static str = "sweep.stamp";
    const TARGET_PERCENT: u64 = 90;
    const WHOLE_PERCENT: u64 = 100;

    /// A sweep that keeps a folder under `budget` bytes, as of `now`.
    #[must_use]
    pub const fn new(budget: u64, now: SystemTime) -> Self {
        Self { budget, now }
    }

    /// Whether `folder` was last swept at least [`Self::INTERVAL`] ago,
    /// or never.
    #[must_use]
    pub fn is_due(&self, folder: &PrivateFolder) -> bool {
        Self::last_time(folder).is_none_or(|swept| self.at_least(swept, Self::INTERVAL))
    }

    /// The totals the last sweep of `folder` wrote, if any.
    #[must_use]
    pub fn last(folder: &PrivateFolder) -> Option<SweepReport> {
        let text = Self::stamp_text(folder).ok()?;
        SweepReport::from_line(&text)
    }

    /// Sweep `folder` now and record it in `sweep.stamp`.
    ///
    /// # Errors
    /// The OS error of listing the folder or writing the stamp. A file
    /// another process removed first counts as removed.
    pub fn run(&self, folder: &PrivateFolder) -> io::Result<SweepReport> {
        let listing = CacheListing::of(folder.path())?;
        let partials_removed = self.remove_old_partials(&listing.files);
        let mut entries = Self::entries_oldest_first(&listing.files);
        let evicted = self.evict(&mut entries);
        listing.remove_empty_folders();
        let report = Self::report(&entries, evicted, partials_removed);
        Self::write_stamp(folder, report)?;
        Ok(report)
    }

    fn at_least(&self, then: SystemTime, age: Duration) -> bool {
        self.now
            .duration_since(then)
            .map_or(true, |elapsed| elapsed >= age)
    }

    /// When `folder` was last swept, if ever.
    pub(crate) fn last_time(folder: &PrivateFolder) -> Option<SystemTime> {
        let file = folder.open_file(Path::new(Self::STAMP)).ok()?;
        file.metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
    }

    fn stamp_text(folder: &PrivateFolder) -> io::Result<String> {
        let file = folder.open_file(Path::new(Self::STAMP))?;
        let limit = u64::try_from(SweepReport::maximum_text_bytes()).unwrap_or(u64::MAX);
        let mut text = String::new();
        file.take(limit).read_to_string(&mut text)?;
        Ok(text)
    }

    fn write_stamp(folder: &PrivateFolder, report: SweepReport) -> io::Result<()> {
        let mut file = folder.replace_file(Path::new(Self::STAMP))?;
        file.write_all(report.to_line().as_bytes())
    }

    fn remove_old_partials(&self, files: &[CacheFile]) -> u64 {
        let old = files.iter().filter(|file| self.is_old_partial(file));
        let removed = old.filter(|file| Self::remove(file));
        Self::count(removed)
    }

    fn is_old_partial(&self, file: &CacheFile) -> bool {
        file.kind == CacheFileKind::Partial && self.at_least(file.modified, Self::PARTIAL_AGE)
    }

    fn entries_oldest_first(files: &[CacheFile]) -> Vec<CacheFile> {
        let mut entries: Vec<CacheFile> = files
            .iter()
            .filter(|file| file.kind == CacheFileKind::Entry)
            .cloned()
            .collect();
        entries.sort_by_key(|entry| entry.modified);
        entries
    }

    fn evict(&self, entries: &mut Vec<CacheFile>) -> u64 {
        let mut total = Self::total(entries);
        let mut evicted = 0;
        while total > self.target() && !entries.is_empty() {
            let oldest = entries.remove(0);
            total -= oldest.bytes;
            evicted += u64::from(Self::remove(&oldest));
        }
        evicted
    }

    fn target(&self) -> u64 {
        self.budget / Self::WHOLE_PERCENT * Self::TARGET_PERCENT
            + self.budget % Self::WHOLE_PERCENT * Self::TARGET_PERCENT / Self::WHOLE_PERCENT
    }

    fn remove(file: &CacheFile) -> bool {
        match fs::remove_file(&file.path) {
            Ok(()) => true,
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        }
    }

    fn report(entries: &[CacheFile], evicted: u64, partials_removed: u64) -> SweepReport {
        SweepReport {
            used_bytes: Self::total(entries),
            entries: Self::count(entries.iter()),
            evicted,
            partials_removed,
        }
    }

    fn total(entries: &[CacheFile]) -> u64 {
        entries.iter().map(|entry| entry.bytes).sum()
    }

    fn count<T>(items: impl Iterator<Item = T>) -> u64 {
        u64::try_from(items.count()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests;
