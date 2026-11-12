use fylo_vfs::File;
use std::fs::TryLockError;
use std::io;

/// The whole-file advisory lock a root lease holds on its lock file.
///
/// `std`'s `File::try_lock` is not built for Android, where it always answers
/// `Unsupported`, so there FYLO calls `flock(2)` itself. Every other target
/// keeps `std`'s lock.
pub(super) struct KernelFileLock;

impl KernelFileLock {
    /// Take the exclusive lock without waiting.
    #[cfg(not(target_os = "android"))]
    pub(super) fn try_lock(file: &File) -> Result<(), TryLockError> {
        file.try_lock()
    }

    /// Release the lock.
    #[cfg(not(target_os = "android"))]
    pub(super) fn unlock(file: &File) -> io::Result<()> {
        file.unlock()
    }

    /// Take the exclusive lock without waiting.
    #[cfg(target_os = "android")]
    pub(super) fn try_lock(file: &File) -> Result<(), TryLockError> {
        rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(Self::refusal)
    }

    /// Release the lock.
    #[cfg(target_os = "android")]
    pub(super) fn unlock(file: &File) -> io::Result<()> {
        rustix::fs::flock(file, rustix::fs::FlockOperation::Unlock).map_err(io::Error::from)
    }

    #[cfg(target_os = "android")]
    fn refusal(errno: rustix::io::Errno) -> TryLockError {
        if errno == rustix::io::Errno::WOULDBLOCK {
            return TryLockError::WouldBlock;
        }
        TryLockError::Error(io::Error::from(errno))
    }
}
