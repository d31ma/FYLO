use std::path::{Path, PathBuf};

use super::disk_description::DiskDescription;

/// What the system-disk rule asks the OS (ADR 0032 section 2). The real
/// answers come from [`super::disk_reader::DiskReader`]; tests give their
/// own.
pub(crate) trait DiskSource {
    /// The disk that holds the running OS.
    fn system(&self) -> Result<DiskDescription, String>;

    /// The disk that holds `path` (or its nearest existing parent).
    fn describe(&self, path: &Path) -> Result<DiskDescription, String>;

    /// The files of the disk images `disk` is made from; none for a real
    /// disk.
    fn images(&self, disk: &DiskDescription) -> Result<Vec<PathBuf>, String>;

    /// Whether `path` is on a network filesystem.
    fn remote(&self, path: &Path) -> Result<bool, String>;
}
