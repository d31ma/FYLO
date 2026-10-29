//! How a process writing a `.fxdb` file coordinates with the others
//! (ADR 0016 section 7).
//!
//! On a filesystem whose byte-range locks are qualified and work, every
//! writer holds the lease byte shared and several processes write. Anywhere
//! else, or when the caller asks for exclusive mode as `SQLite`'s
//! `locking_mode=EXCLUSIVE` does, one process writes under the lock-free
//! lease. Readers need neither.
//!
//! The two modes exclude each other. A lease writer whose filesystem grants
//! locks at all also holds the lease byte exclusive, so it waits out lock
//! writers and they wait it out. A lock writer refuses to start while a
//! lease is held, and takes over one a crashed writer left. What no rule can
//! cover is a host whose filesystem refuses every lock sharing the file with
//! a host that uses them; ADR 0016 lists that as unsupported.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::{
    ByteLockGuard, ByteRangeLockFile, ContainerError, ContainerFile, ContainerLease,
    ContainerLeaseError, ContainerLeaseTiming, FilesystemInfo, LockByte, LockMode,
};

/// Options for opening a `.fxdb` file to write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerWriteOptions {
    /// How long to wait for other writers before failing with `Busy`.
    pub busy_timeout: Duration,
    /// Write under the lease even where byte-range locks are qualified.
    pub exclusive: bool,
    /// Lease timing, where the lease is used.
    pub lease_timing: ContainerLeaseTiming,
}

impl Default for ContainerWriteOptions {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_secs(5),
            exclusive: false,
            lease_timing: ContainerLeaseTiming::default(),
        }
    }
}

/// What lets this process write. Keep it for as long as the container is
/// written; dropping it gives the right up.
#[derive(Debug)]
pub enum ContainerWriteAccess {
    /// Byte-range locks: the lease byte held shared, alongside other writers.
    SharedLocks {
        /// The shared lease byte.
        lease_byte: ByteLockGuard,
    },
    /// The lock-free lease: this process is the only writer.
    Lease {
        /// The lease file.
        lease: ContainerLease,
        /// The lease byte held exclusive, where the filesystem grants locks.
        lease_byte: Option<ByteLockGuard>,
    },
}

impl ContainerWriteAccess {
    /// Check, before a commit, that this process may still write.
    ///
    /// # Errors
    ///
    /// Returns `Lease(Lost)` after another process took the lease over.
    pub fn assert_writable(&self) -> Result<(), ContainerError> {
        match self {
            Self::SharedLocks { .. } => Ok(()),
            Self::Lease { lease, .. } => lease.assert_owned().map_err(ContainerError::Lease),
        }
    }

    /// Hold the file as the only writer process, for an operation that
    /// needs it to itself (`fylo compact --shrink`): a writer sharing the
    /// file takes its lease byte exclusive, waiting up to `wait` for the
    /// other writers to let it go; under the lease this process already is
    /// the only writer. [`Self::share`] undoes it.
    ///
    /// # Errors
    ///
    /// Returns `Busy` when other writers keep the file past `wait`, with the
    /// byte held shared again, or the lock failure, after which this process
    /// holds no lease byte and must stop writing.
    pub fn hold_exclusively(&mut self, wait: Duration) -> Result<(), ContainerError> {
        self.lease_mode(LockMode::Exclusive, wait)
    }

    /// Share the file with other writers again after
    /// [`Self::hold_exclusively`].
    ///
    /// # Errors
    ///
    /// Returns `Busy` or the lock failure, after which this process holds no
    /// lease byte and must stop writing.
    pub fn share(&mut self, wait: Duration) -> Result<(), ContainerError> {
        self.lease_mode(LockMode::Shared, wait)
    }

    fn lease_mode(&mut self, mode: LockMode, wait: Duration) -> Result<(), ContainerError> {
        match self {
            Self::SharedLocks { lease_byte } => {
                lease_byte.change_mode(mode, wait).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::TimedOut {
                        ContainerError::Busy
                    } else {
                        ContainerError::Io(error)
                    }
                })
            }
            Self::Lease { .. } => Ok(()),
        }
    }

    /// Renew the lease, if this process writes under one. Call it at most a
    /// third of the lease's takeover time apart.
    ///
    /// # Errors
    ///
    /// Returns `Lease(Lost)` after another process took the lease over.
    pub fn renew(&mut self) -> Result<(), ContainerError> {
        match self {
            Self::SharedLocks { .. } => Ok(()),
            Self::Lease { lease, .. } => lease.renew().map_err(ContainerError::Lease),
        }
    }
}

impl ContainerFile {
    /// Open an existing container to write, coordinating with other writers
    /// the way its filesystem allows.
    ///
    /// # Errors
    ///
    /// Returns `Busy` when other writers keep it past the busy timeout,
    /// `Sealed` for a sealed file, or an open, read or lock failure.
    pub fn open_writer(
        path: &Path,
        options: ContainerWriteOptions,
    ) -> Result<(Self, ContainerWriteAccess), ContainerError> {
        let handle = ByteRangeLockFile::open(path)?;
        // A sealed file is never written: it is refused before a lease or
        // a lock byte is taken for it.
        ContainerFile::refuse_sealed(handle.file())?;
        let qualified = FilesystemInfo::of(handle.file())?.byte_range_locks_qualified();
        let locks_granted = handle.probe();
        let deadline = Instant::now() + options.busy_timeout;
        let access = if qualified && locks_granted && !options.exclusive {
            clear_lease(path, options, deadline)?;
            ContainerWriteAccess::SharedLocks {
                lease_byte: lock_until(&handle, LockMode::Shared, deadline)?,
            }
        } else {
            let lease = ContainerLease::acquire(path, options.lease_timing, remaining(deadline))
                .map_err(lease_busy)?;
            let lease_byte = if locks_granted {
                match lock_until(&handle, LockMode::Exclusive, deadline) {
                    Ok(guard) => Some(guard),
                    Err(error) => {
                        lease.release().map_err(ContainerError::Lease)?;
                        return Err(error);
                    }
                }
            } else {
                None
            };
            // Writer and allocation bytes are then decided in this process
            // alone, so its threads still exclude each other.
            handle.set_process_local(true);
            ContainerWriteAccess::Lease { lease, lease_byte }
        };
        Ok((Self::from_handle(handle, true)?, access))
    }
}

/// Wait for a lease writer to finish, taking over a lease left by a crash,
/// and hand the lease back free.
fn clear_lease(
    path: &Path,
    options: ContainerWriteOptions,
    deadline: Instant,
) -> Result<(), ContainerError> {
    if ContainerLease::held_generation(path)?.is_none() {
        return Ok(());
    }
    ContainerLease::acquire(path, options.lease_timing, remaining(deadline))
        .map_err(lease_busy)?
        .release()
        .map_err(ContainerError::Lease)
}

fn lock_until(
    handle: &ByteRangeLockFile,
    mode: LockMode,
    deadline: Instant,
) -> Result<ByteLockGuard, ContainerError> {
    handle
        .lock(LockByte::LEASE, mode, remaining(deadline))
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::TimedOut {
                ContainerError::Busy
            } else {
                ContainerError::Io(error)
            }
        })
}

fn lease_busy(error: ContainerLeaseError) -> ContainerError {
    match error {
        ContainerLeaseError::Busy { .. } => ContainerError::Busy,
        other => ContainerError::Lease(other),
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

#[cfg(test)]
mod tests;
