//! Byte-range locks on a `.fxdb` file (ADR 0016 section 7).
//!
//! Local processes sharing one `.fxdb` coordinate through OS locks on single
//! bytes in a range reserved far beyond any data offset, starting at 2⁶²: one
//! byte for the database lease, one for extent allocation, and one writer byte
//! per collection, indexed by its directory block. A lock beyond the end of the
//! file does not extend it. Readers take no lock.
//!
//! The OS decides between processes. Inside one process a registry decides,
//! because the OS owners differ per platform: Linux open-file-description locks
//! belong to one open, Windows locks to one handle, and macOS `fcntl` locks to
//! the whole process, where a second thread asking for a byte its process holds
//! is granted it and closing any descriptor for the file drops them all. So,
//! as `SQLite` does, every [`ByteRangeLockFile`] for one file in a process shares
//! one handle, the registry counts holders per byte, and a duplicate descriptor
//! is closed only once the process holds no lock on the file.
//!
//! Do I/O on a locked `.fxdb` through [`ByteRangeLockFile::file`]. On macOS,
//! opening and closing the file by path anywhere else in the process releases
//! its locks.
//!
//! Waiting polls: [`ByteRangeLockFile::lock`] retries the non-waiting request
//! with a backoff of up to [`MAX_POLL_INTERVAL`], and is woken at once by a
//! release inside the process. A blocking kernel wait (`F_OFD_SETLKW`) has no
//! timeout and would hold a request the registry cannot see.

use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

mod os;
#[cfg(test)]
mod tests;

use os::FileKey;

/// First byte of the reserved lock range.
pub const LOCK_RANGE_START: u64 = 1 << 62;
/// Offset of collection writer bytes from [`LOCK_RANGE_START`]. The bytes
/// below it are kept for further database-wide locks.
pub const COLLECTION_WRITER_START: u64 = 4096;
/// The longest a waiting [`ByteRangeLockFile::lock`] sleeps between tries.
pub const MAX_POLL_INTERVAL: Duration = Duration::from_millis(1);
const FIRST_POLL_INTERVAL: Duration = Duration::from_micros(20);

/// One reserved lock byte in a `.fxdb` file.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LockByte(u64);

impl LockByte {
    /// The database lease. `fylo serve` holds it exclusively; local processes
    /// sharing the file hold it shared.
    pub const LEASE: Self = Self(LOCK_RANGE_START);
    /// Extent allocation: taking a free extent or growing the file.
    pub const ALLOCATION: Self = Self(LOCK_RANGE_START + 1);

    /// The writer byte of the collection in directory block `directory_block`.
    #[must_use]
    pub fn collection_writer(directory_block: u32) -> Self {
        Self(LOCK_RANGE_START + COLLECTION_WRITER_START + u64::from(directory_block))
    }

    /// The byte's file offset.
    #[must_use]
    pub const fn offset(self) -> u64 {
        self.0
    }
}

impl fmt::Display for LockByte {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 - LOCK_RANGE_START {
            0 => f.write_str("lease"),
            1 => f.write_str("allocation"),
            n if n >= COLLECTION_WRITER_START => {
                write!(f, "writer {}", n - COLLECTION_WRITER_START)
            }
            n => write!(f, "reserved {n}"),
        }
    }
}

/// How a byte is held.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockMode {
    /// Any number of shared holders, and no exclusive one.
    Shared,
    /// One holder.
    Exclusive,
}

#[derive(Clone, Copy, Debug, Default)]
struct Holders {
    shared: u32,
    exclusive: bool,
}

#[derive(Debug, Default)]
struct LockState {
    held: HashMap<u64, Holders>,
    /// Descriptors for this file opened while it was locked, closed once it
    /// is not.
    deferred_closes: Vec<File>,
}

#[derive(Debug)]
struct SharedFile {
    file: File,
    state: Mutex<LockState>,
    released: Condvar,
}

impl SharedFile {
    fn state(&self) -> MutexGuard<'_, LockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Open files by identity, with how many handles and guards use each.
type Registry = HashMap<FileKey, (Arc<SharedFile>, usize)>;

fn registry() -> MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// A `.fxdb` file opened for byte-range locking.
///
/// Every handle to one file in a process shares one OS handle, which
/// [`Self::file`] lends for I/O. Clones share it too.
#[derive(Debug)]
pub struct ByteRangeLockFile {
    key: FileKey,
    // Taken in `drop`, so the last user closes the handle under the registry
    // lock and no newer open of the file can lock it first.
    shared: Option<Arc<SharedFile>>,
}

impl ByteRangeLockFile {
    /// Open an existing file for reading, writing and locking.
    ///
    /// # Errors
    ///
    /// Returns the open failure, or a failure to read the file's identity.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let key = os::file_key(&file, path)?;
        let mut registry = registry();
        if let Some((shared, users)) = registry.get_mut(&key) {
            *users += 1;
            let shared = Arc::clone(shared);
            drop(registry);
            let mut state = shared.state();
            if state.held.is_empty() {
                drop(file);
            } else {
                state.deferred_closes.push(file);
            }
            drop(state);
            return Ok(Self {
                key,
                shared: Some(shared),
            });
        }
        let shared = Arc::new(SharedFile {
            file,
            state: Mutex::default(),
            released: Condvar::new(),
        });
        registry.insert(key.clone(), (Arc::clone(&shared), 1));
        Ok(Self {
            key,
            shared: Some(shared),
        })
    }

    fn shared(&self) -> &SharedFile {
        self.shared
            .as_deref()
            .expect("a byte-range lock file is only emptied by drop")
    }

    /// The file, for reads and writes. Every handle to it in this process
    /// returns the same one.
    #[must_use]
    pub fn file(&self) -> &File {
        &self.shared().file
    }

    /// Take `byte` without waiting.
    ///
    /// Returns `None` when another holder in this process or another process
    /// holds it in a conflicting mode. A holder of a shared byte cannot
    /// upgrade it: asking for it exclusively conflicts with itself.
    ///
    /// # Errors
    ///
    /// Returns an OS failure other than a conflict, such as `ENOLCK` from a
    /// filesystem with no lock manager.
    pub fn try_lock(&self, byte: LockByte, mode: LockMode) -> io::Result<Option<ByteLockGuard>> {
        let shared = self.shared();
        let mut state = shared.state();
        if !Self::try_lock_in(shared, &mut state, byte, mode)? {
            return Ok(None);
        }
        drop(state);
        Ok(Some(ByteLockGuard {
            file: self.clone(),
            byte,
            mode,
            held: true,
        }))
    }

    /// Take `byte`, waiting up to `timeout` for it.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::TimedOut`] when the byte is still held at the
    /// deadline, or an OS failure as [`Self::try_lock`] does.
    pub fn lock(
        &self,
        byte: LockByte,
        mode: LockMode,
        timeout: Duration,
    ) -> io::Result<ByteLockGuard> {
        let deadline = Instant::now() + timeout;
        let mut interval = FIRST_POLL_INTERVAL;
        let shared = self.shared();
        let mut state = shared.state();
        loop {
            if Self::try_lock_in(shared, &mut state, byte, mode)? {
                drop(state);
                return Ok(ByteLockGuard {
                    file: self.clone(),
                    byte,
                    mode,
                    held: true,
                });
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out after {timeout:?} waiting for the {byte} lock"),
                ));
            }
            state = shared
                .released
                .wait_timeout(state, interval.min(deadline - now))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            interval = (interval * 2).min(MAX_POLL_INTERVAL);
        }
    }

    fn try_lock_in(
        shared: &SharedFile,
        state: &mut LockState,
        byte: LockByte,
        mode: LockMode,
    ) -> io::Result<bool> {
        let holders = state.held.get(&byte.0).copied().unwrap_or_default();
        let next = match mode {
            LockMode::Exclusive => {
                if holders.exclusive || holders.shared > 0 {
                    return Ok(false);
                }
                if !os::try_lock(&shared.file, byte.0, true)? {
                    return Ok(false);
                }
                Holders {
                    shared: 0,
                    exclusive: true,
                }
            }
            LockMode::Shared => {
                if holders.exclusive {
                    return Ok(false);
                }
                if holders.shared == 0 && !os::try_lock(&shared.file, byte.0, false)? {
                    return Ok(false);
                }
                Holders {
                    shared: holders.shared + 1,
                    exclusive: false,
                }
            }
        };
        state.held.insert(byte.0, next);
        Ok(true)
    }

    fn release(&self, byte: LockByte, mode: LockMode) -> io::Result<()> {
        let shared = self.shared();
        let mut state = shared.state();
        let Some(holders) = state.held.get_mut(&byte.0) else {
            return Ok(());
        };
        let last = match mode {
            LockMode::Exclusive => true,
            LockMode::Shared => {
                holders.shared -= 1;
                holders.shared == 0
            }
        };
        if !last {
            return Ok(());
        }
        state.held.remove(&byte.0);
        // Released in the registry even when the OS refuses: the handle
        // still drops whatever the OS kept when its last user closes it.
        let result = os::unlock(&shared.file, byte.0);
        if state.held.is_empty() {
            state.deferred_closes.clear();
        }
        drop(state);
        shared.released.notify_all();
        result
    }
}

impl Clone for ByteRangeLockFile {
    fn clone(&self) -> Self {
        if let Some((_, users)) = registry().get_mut(&self.key) {
            *users += 1;
        }
        Self {
            key: self.key.clone(),
            shared: self.shared.clone(),
        }
    }
}

impl Drop for ByteRangeLockFile {
    fn drop(&mut self) {
        let mut registry = registry();
        let Some((_, users)) = registry.get_mut(&self.key) else {
            return;
        };
        *users -= 1;
        if *users == 0 {
            // Both references go while the registry is held, which closes the
            // handle before any later open of this file can lock through a
            // new one.
            self.shared = None;
            registry.remove(&self.key);
        }
    }
}

/// A held lock byte, released when dropped.
#[must_use = "dropping the guard releases the lock"]
#[derive(Debug)]
pub struct ByteLockGuard {
    file: ByteRangeLockFile,
    byte: LockByte,
    mode: LockMode,
    held: bool,
}

impl ByteLockGuard {
    /// The byte held.
    pub fn byte(&self) -> LockByte {
        self.byte
    }

    /// How it is held.
    pub fn mode(&self) -> LockMode {
        self.mode
    }

    /// Release the byte now, reporting an OS failure to unlock.
    ///
    /// # Errors
    ///
    /// Returns the OS unlock failure. The byte is released in this process
    /// either way.
    pub fn unlock(mut self) -> io::Result<()> {
        self.held = false;
        self.file.release(self.byte, self.mode)
    }
}

impl Drop for ByteLockGuard {
    fn drop(&mut self) {
        if self.held {
            let _ = self.file.release(self.byte, self.mode);
        }
    }
}
