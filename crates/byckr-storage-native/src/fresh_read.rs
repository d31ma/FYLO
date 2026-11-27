use byckr_vfs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::path::Path;

/// Read a small coordination file so that a network file system cannot
/// serve old bytes for longer than one attribute-cache period (ADR 0033
/// section 4): the file is opened again on every call (never a cached
/// `stat` alone), its type and size come from the open handle, and on macOS
/// the client's data cache is turned off for that handle (`F_NOCACHE`),
/// which otherwise served data up to 5 s old on an NFS mount in our
/// measurement, and 1.6 s with it off. No link is followed.
///
/// Every call reads the whole file over the network on a share, so this is
/// for files that must be current (lease records, marks, small logs), not
/// for bulk data.
pub(crate) struct FreshRead;

impl FreshRead {
    /// The bytes of the regular file `path`, at most `limit` of them.
    ///
    /// # Errors
    ///
    /// `NotFound` when nothing is at `path`; `InvalidData` for a link, a
    /// folder or a file over `limit`; any other error of the open or read.
    pub(crate) fn bytes(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
        let mut file = Self::open(path)?;
        Self::require_small_file(&file, limit)?;
        Self::uncached(&file);
        let mut bytes = Vec::new();
        (&mut file).take(limit).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn require_small_file(file: &File, limit: u64) -> std::io::Result<()> {
        let metadata = file.metadata()?;
        if metadata.is_file() && metadata.len() <= limit {
            return Ok(());
        }
        Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "not a regular file within the size limit",
        ))
    }

    #[cfg(target_vendor = "apple")]
    fn uncached(file: &File) {
        let _ = rustix::fs::fcntl_nocache(file, true);
    }

    #[cfg(not(target_vendor = "apple"))]
    fn uncached(_file: &File) {}

    #[cfg(all(unix, not(target_family = "wasm")))]
    fn open(path: &Path) -> std::io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }

    #[cfg(not(all(unix, not(target_family = "wasm"))))]
    fn open(path: &Path) -> std::io::Result<File> {
        let is_link = byckr_vfs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        if is_link {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "a coordination file must not be a link",
            ));
        }
        OpenOptions::new().read(true).open(path)
    }
}
