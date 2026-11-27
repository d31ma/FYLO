use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The short-lived files of one probe run. Every name it hands out, and the
/// `AppleDouble` file macOS may make beside it, is removed when it is dropped,
/// so a probe leaves nothing behind whether it passed or failed.
pub(crate) struct ProbeFiles {
    directory: PathBuf,
    reserved: Vec<PathBuf>,
}

impl ProbeFiles {
    const FILE_PREFIX: &'static str = ".fylo-filesystem-probe-";
    const FILE_SUFFIX: &'static str = ".tmp";
    const APPLE_DOUBLE_PREFIX: &'static str = "._";

    pub(crate) fn new(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
            reserved: Vec::new(),
        }
    }

    pub(crate) fn reserve_path(&mut self) -> PathBuf {
        let path = self.directory.join(Self::unique_name());
        self.reserved.push(Self::apple_double_of(&path));
        self.reserved.push(path.clone());
        path
    }

    pub(crate) fn create_new(&mut self) -> std::io::Result<(PathBuf, File)> {
        let path = self.reserve_path();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok((path, file))
    }

    /// The `._<name>` file macOS writes beside `path` when the file system
    /// cannot keep extended attributes itself.
    pub(crate) fn apple_double_of(path: &Path) -> PathBuf {
        let mut name = OsString::from(Self::APPLE_DOUBLE_PREFIX);
        name.push(path.file_name().unwrap_or_default());
        path.with_file_name(name)
    }

    fn unique_name() -> String {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let nanoseconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        format!(
            "{}{}-{nanoseconds}-{}{}",
            Self::FILE_PREFIX,
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
            Self::FILE_SUFFIX
        )
    }
}

impl Drop for ProbeFiles {
    fn drop(&mut self) {
        for path in &self.reserved {
            let _ = std::fs::remove_file(path);
        }
    }
}
