use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::cache_file::CacheFile;
use super::cache_file_kind::CacheFileKind;
use super::cache_listing::CacheListing;
use super::cache_sweep::CacheSweep;
use super::private_folder::PrivateFolder;

/// What a temp cache folder holds now, for `fylo cache status`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheUsage {
    /// The cache folder.
    pub dir: PathBuf,
    /// Whether the folder exists.
    pub exists: bool,
    /// Bytes of finished entries.
    pub used_bytes: u64,
    /// Finished entries.
    pub entries: u64,
    /// Partial files: entries being written, or left by a crash.
    pub partial_files: u64,
    /// When the folder was last swept.
    pub last_sweep: Option<SystemTime>,
}

impl CacheUsage {
    /// A folder that does not exist.
    #[must_use]
    pub fn missing(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            exists: false,
            used_bytes: 0,
            entries: 0,
            partial_files: 0,
            last_sweep: None,
        }
    }

    /// Count what `folder` holds.
    ///
    /// # Errors
    /// The OS error of listing the folder.
    pub fn of(folder: &PrivateFolder) -> io::Result<Self> {
        let listing = CacheListing::of(folder.path())?;
        let entries = Self::of_kind(&listing.files, CacheFileKind::Entry);
        Ok(Self {
            dir: folder.path().to_path_buf(),
            exists: true,
            used_bytes: entries.iter().map(|entry| entry.bytes).sum(),
            entries: Self::count(entries.len()),
            partial_files: Self::count(Self::of_kind(&listing.files, CacheFileKind::Partial).len()),
            last_sweep: CacheSweep::last_time(folder),
        })
    }

    fn of_kind(files: &[CacheFile], kind: CacheFileKind) -> Vec<&CacheFile> {
        files.iter().filter(|file| file.kind == kind).collect()
    }

    fn count(length: usize) -> u64 {
        u64::try_from(length).unwrap_or(u64::MAX)
    }
}
