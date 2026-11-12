use super::kernel_file_lock::KernelFileLock;
use super::lease_backoff::LeaseBackoff;
use super::root_lease_inner::RootLeaseInner;
use crate::{FilesystemProfile, NativeStorageError, RootFilesystem};
use fylo_vfs::{File, OpenOptions};
use std::fs::TryLockError;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Instant;

/// The right to take a busy root next.
///
/// A process that waits for a root first takes this lock, kept in the file
/// `.<root>.fylo-root-turn.lock` beside the root's owner lock, and keeps it
/// until it owns the root. A process that starts later meets this lock
/// before the owner lock, so it cannot take the root in the short gap
/// between the owner's release and the waiter's next try. Without it a
/// waiter that tries every few milliseconds can lose every gap to new
/// processes and wait out its whole deadline. The kernel drops the lock when
/// its process ends, so a waiter that crashed never blocks the root.
pub(super) struct RootTurn {
    file: File,
}

impl RootTurn {
    const FILE_SUFFIX: &str = "fylo-root-turn.lock";

    /// Wait until `wait_until` for the turn to take `root`. `None` when the
    /// root does not exist yet or the time ran out: the caller then tries
    /// the root without a turn.
    ///
    /// # Errors
    /// Refuses a root on a network filesystem (ADR 0029) before the turn
    /// file is created, and returns a turn file that cannot be opened or
    /// locked.
    pub(super) fn wait(
        root: &Path,
        profile: FilesystemProfile,
        wait_until: Instant,
    ) -> Result<Option<Self>, NativeStorageError> {
        RootFilesystem::admit(root, profile)?;
        let file = Self::open(root)?;
        file.map(|file| Self::poll(file, wait_until))
            .transpose()
            .map(Option::flatten)
    }

    fn open(root: &Path) -> Result<Option<File>, NativeStorageError> {
        let Ok(canonical) = fylo_vfs::canonicalize(root) else {
            return Ok(None);
        };
        let path = RootLeaseInner::sibling_path(&canonical, Self::FILE_SUFFIX)?;
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map(Some)
            .map_err(NativeStorageError::io)
    }

    fn poll(file: File, wait_until: Instant) -> Result<Option<Self>, NativeStorageError> {
        let mut backoff = LeaseBackoff::until(wait_until);
        let mut taken = Self::try_take(&file)?;
        while !taken && backoff.wait() {
            taken = Self::try_take(&file)?;
        }
        Ok(taken.then_some(Self { file }))
    }

    /// Whether this process now holds the turn. A filesystem without
    /// advisory locks gives no turn to wait for, so it counts as taken.
    fn try_take(file: &File) -> Result<bool, NativeStorageError> {
        match KernelFileLock::try_lock(file) {
            Ok(()) => Ok(true),
            Err(TryLockError::WouldBlock) => Ok(false),
            Err(TryLockError::Error(error)) if error.kind() == ErrorKind::PermissionDenied => {
                Ok(false)
            }
            Err(TryLockError::Error(error)) if error.kind() == ErrorKind::Unsupported => Ok(true),
            Err(TryLockError::Error(error)) => Err(NativeStorageError::io(error)),
        }
    }
}

impl Drop for RootTurn {
    fn drop(&mut self) {
        let _ = KernelFileLock::unlock(&self.file);
    }
}
