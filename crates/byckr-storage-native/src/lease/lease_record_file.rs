use super::lease_record::LeaseRecord;
use crate::NativeStorageError;
use byckr_vfs::{File, OpenOptions};
use std::io::{ErrorKind, Seek, SeekFrom, Write};
use std::path::Path;

/// The bytes of a lease record on disk, read and written so that a
/// network file system cannot serve an old heartbeat for longer than one
/// attribute-cache period (ADR 0033 section 4).
///
/// Every write rewrites the same file in place (open, write, set length,
/// fsync, close), never a new file renamed over it: an NFS client keeps the
/// name-to-file mapping for its whole directory-cache time (30 s by default
/// on Linux), and a renamed record would hide behind it. Every read opens the
/// file again and reads it; on macOS the read also turns off the client's
/// data cache for that file (`F_NOCACHE`), which otherwise served a record up
/// to 5 s old on an NFS version 3 mount in our measurement, and 1.6 s with
/// it off.
/// No link is followed, for reads or writes.
pub(super) struct LeaseRecordFile;

impl LeaseRecordFile {
    /// `None` only when no record exists. A link, a folder, an oversized
    /// file or a failed read gives empty bytes: an unreadable record. The
    /// read itself is `FreshRead`'s.
    pub(super) fn read(path: &Path) -> Option<Vec<u8>> {
        match crate::fresh_read::FreshRead::bytes(path, LeaseRecord::MAXIMUM_BYTES) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(_) => Some(Vec::new()),
        }
    }

    /// Rewrite the record in place and flush it.
    pub(super) fn write(path: &Path, bytes: &[u8]) -> Result<(), NativeStorageError> {
        let mut file = Self::open_for_write(path).map_err(NativeStorageError::io)?;
        file.seek(SeekFrom::Start(0))
            .map_err(NativeStorageError::io)?;
        file.write_all(bytes).map_err(NativeStorageError::io)?;
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        file.set_len(length).map_err(NativeStorageError::io)?;
        crate::sync_handle(&file).map_err(NativeStorageError::io)
    }

    #[cfg(all(unix, not(target_family = "wasm")))]
    fn open_for_write(path: &Path) -> std::io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }

    #[cfg(not(all(unix, not(target_family = "wasm"))))]
    fn open_for_write(path: &Path) -> std::io::Result<File> {
        Self::refuse_link(path)?;
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
    }

    /// Without `O_NOFOLLOW` a link is refused by looking first; a missing
    /// record passes, as the open then creates or reports it.
    #[cfg(not(all(unix, not(target_family = "wasm"))))]
    fn refuse_link(path: &Path) -> std::io::Result<()> {
        let is_link = byckr_vfs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        if is_link {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                "a lease record must not be a link",
            ));
        }
        Ok(())
    }
}
