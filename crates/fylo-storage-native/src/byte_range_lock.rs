//! Byte-range locks on a `.fxdb` file (ADR 0016 section 7).
//!
//! Local processes sharing one `.fxdb` coordinate through OS locks on single
//! bytes in a range reserved far beyond any data offset, starting at 2⁶²: one
//! byte for the database lease, one for extent allocation, and two bytes per
//! collection, indexed by its directory block: its writer byte and its
//! compactor byte. A lock beyond the end of the file does not extend it.
//! Readers take no lock.
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

mod filesystem;
mod os;
#[cfg(test)]
mod tests;

pub use filesystem::FilesystemInfo;
use os::FileKey;

/// First byte of the reserved lock range.
pub const LOCK_RANGE_START: u64 = 1 << 62;
/// Offset of collection writer bytes from [`LOCK_RANGE_START`]. The bytes
/// below it are kept for further database-wide locks.
pub const COLLECTION_WRITER_START: u64 = 4096;
/// Offset of collection compactor bytes from [`LOCK_RANGE_START`], past
/// every writer byte a directory block number can name.
pub const COLLECTION_COMPACTOR_START: u64 = COLLECTION_WRITER_START + (1 << 32);
/// A reserved byte that [`ByteRangeLockFile::probe`] takes and releases.
const PROBE: u64 = LOCK_RANGE_START + 2;
/// The longest a waiting [`ByteRangeLockFile::lock`] sleeps between tries.
pub const MAX_POLL_INTERVAL: Duration = Duration::from_millis(1);
const FIRST_POLL_INTERVAL: Duration = Duration::from_micros(20);

/// One reserved lock byte in a `.fxdb` file.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LockByte(u64);

impl LockByte {
    /// The database lease. Every process holds it shared while it has the file
    /// open; an operation that needs the file to itself (`compact --shrink`,
    /// an in-place `pack --sealed`) takes it exclusive.
    pub const LEASE: Self = Self(LOCK_RANGE_START);
    /// Extent allocation: taking a free extent or growing the file.
    pub const ALLOCATION: Self = Self(LOCK_RANGE_START + 1);

    /// The writer byte of the collection in directory block `directory_block`.
    #[must_use]
    pub fn collection_writer(directory_block: u32) -> Self {
        Self(LOCK_RANGE_START + COLLECTION_WRITER_START + u64::from(directory_block))
    }

    /// The compactor byte of the collection in directory block
    /// `directory_block`: one compaction of the collection at a time holds
    /// it, while its copy runs without the writer byte (ADR 0015 section 7).
    #[must_use]
    pub fn collection_compactor(directory_block: u32) -> Self {
        Self(LOCK_RANGE_START + COLLECTION_COMPACTOR_START + u64::from(directory_block))
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
            n if n >= COLLECTION_COMPACTOR_START => {
                write!(f, "compactor {}", n - COLLECTION_COMPACTOR_START)
            }
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
    /// The descriptor the OS lock was taken through, if any; it must also be
    /// the one that releases it.
    os: Option<Descriptor>,
}

/// Which of a file's descriptors a request goes through.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Descriptor {
    /// The one the file was first opened with in this process.
    First,
    /// A writable one added when the first was read-only.
    Writable,
}

#[derive(Debug, Default)]
struct LockState {
    held: HashMap<u64, Holders>,
    /// Set by [`ByteRangeLockFile::set_process_local`]: every byte is then
    /// decided in the registry alone, with no OS call.
    process_local: bool,
    /// Descriptors for this file opened while it was locked, closed once it
    /// is not.
    deferred_closes: Vec<File>,
}

#[derive(Debug)]
struct SharedFile {
    file: File,
    first_writable: bool,
    /// Set by the first writable open after a read-only one, and kept until
    /// the last handle closes, since closing it could drop locks on macOS.
    writable: OnceLock<File>,
    state: Mutex<LockState>,
    released: Condvar,
}

impl SharedFile {
    fn state(&self) -> MutexGuard<'_, LockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn descriptor(&self, descriptor: Descriptor) -> &File {
        match descriptor {
            Descriptor::First => &self.file,
            Descriptor::Writable => self.writable.get().unwrap_or(&self.file),
        }
    }

    /// The writable descriptor, when the process has one.
    fn writable(&self) -> Option<Descriptor> {
        if self.first_writable {
            Some(Descriptor::First)
        } else {
            self.writable.get().map(|_| Descriptor::Writable)
        }
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
    writable: bool,
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
        Self::open_with(path.as_ref(), true)
    }

    /// Open an existing file for reading, on a read-only file or mount too.
    ///
    /// When the process already has the file open, this shares that handle,
    /// so closing it never drops another handle's locks. A read-only handle
    /// takes only the lease byte, and only shared.
    ///
    /// # Errors
    ///
    /// Returns the open failure, or a failure to read the file's identity.
    pub fn open_read_only(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with(path.as_ref(), false)
    }

    fn open_with(path: &Path, writable: bool) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(writable).open(path)?;
        let key = os::file_key(&file, path)?;
        let mut registry = registry();
        if let Some((shared, users)) = registry.get_mut(&key) {
            *users += 1;
            let shared = Arc::clone(shared);
            drop(registry);
            let spare = if writable && !shared.first_writable {
                shared.writable.set(file).err()
            } else {
                Some(file)
            };
            if let Some(file) = spare {
                let mut state = shared.state();
                if state.held.is_empty() {
                    drop(file);
                } else {
                    state.deferred_closes.push(file);
                }
            }
            return Ok(Self {
                key,
                writable,
                shared: Some(shared),
            });
        }
        let shared = Arc::new(SharedFile {
            file,
            first_writable: writable,
            writable: OnceLock::new(),
            state: Mutex::default(),
            released: Condvar::new(),
        });
        registry.insert(key.clone(), (Arc::clone(&shared), 1));
        Ok(Self {
            key,
            writable,
            shared: Some(shared),
        })
    }

    /// The descriptor this handle's requests and I/O go through.
    fn descriptor(&self) -> Descriptor {
        if self.writable {
            self.shared()
                .writable()
                .expect("a writable handle registered a writable descriptor")
        } else {
            Descriptor::First
        }
    }

    /// Whether this handle may write and take every byte.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    fn shared(&self) -> &SharedFile {
        self.shared
            .as_deref()
            .expect("a byte-range lock file is only emptied by drop")
    }

    /// The file, for reads and, from a writable handle, writes. Every
    /// handle to it in this process shares the same descriptors.
    #[must_use]
    pub fn file(&self) -> &File {
        self.shared().descriptor(self.descriptor())
    }

    /// A read-only handle takes the lease byte shared, and nothing else.
    fn check_access(&self, byte: LockByte, mode: LockMode) -> io::Result<()> {
        if self.writable || (byte == LockByte::LEASE && mode == LockMode::Shared) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("a read-only handle cannot take the {byte} lock"),
            ))
        }
    }

    /// Take `byte` without waiting.
    ///
    /// Returns `None` when another holder in this process or another process
    /// holds it in a conflicting mode. A holder of a shared byte cannot
    /// upgrade it: asking for it exclusively conflicts with itself.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::PermissionDenied`] when a read-only handle
    /// asks for any byte but the lease shared, or an OS failure other than a
    /// conflict, such as `ENOLCK` from a filesystem with no lock manager.
    pub fn try_lock(&self, byte: LockByte, mode: LockMode) -> io::Result<Option<ByteLockGuard>> {
        self.check_access(byte, mode)?;
        let shared = self.shared();
        let mut state = shared.state();
        if !Self::try_lock_in(shared, self.descriptor(), &mut state, byte, mode)? {
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
    /// deadline, or a failure as [`Self::try_lock`] does.
    pub fn lock(
        &self,
        byte: LockByte,
        mode: LockMode,
        timeout: Duration,
    ) -> io::Result<ByteLockGuard> {
        self.check_access(byte, mode)?;
        let deadline = Instant::now() + timeout;
        let mut interval = FIRST_POLL_INTERVAL;
        let shared = self.shared();
        let descriptor = self.descriptor();
        let mut state = shared.state();
        loop {
            if Self::try_lock_in(shared, descriptor, &mut state, byte, mode)? {
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
        descriptor: Descriptor,
        state: &mut LockState,
        byte: LockByte,
        mode: LockMode,
    ) -> io::Result<bool> {
        let holders = state.held.get(&byte.0).copied().unwrap_or_default();
        let file = shared.descriptor(descriptor);
        let os = (!state.process_local).then_some(descriptor);
        let next = match mode {
            LockMode::Exclusive => {
                if holders.exclusive || holders.shared > 0 {
                    return Ok(false);
                }
                if os.is_some() && !os::try_lock(file, byte.0, true)? {
                    return Ok(false);
                }
                Holders {
                    shared: 0,
                    exclusive: true,
                    os,
                }
            }
            LockMode::Shared => {
                if holders.exclusive {
                    return Ok(false);
                }
                let os = if holders.shared == 0 {
                    if os.is_some() && !os::try_lock(file, byte.0, false)? {
                        return Ok(false);
                    }
                    os
                } else {
                    holders.os
                };
                Holders {
                    shared: holders.shared + 1,
                    exclusive: false,
                    os,
                }
            }
        };
        state.held.insert(byte.0, next);
        Ok(true)
    }

    /// Decide every byte inside this process alone, with no OS lock, from
    /// the next request on; `false` returns to OS locks.
    ///
    /// This is for a process that writes the file alone under the lock-free
    /// lease (ADR 0016 section 7), where the filesystem's locks are
    /// unqualified or refused: its threads still exclude each other on writer
    /// and allocation bytes. It applies to every handle to the file in the
    /// process. A byte already held keeps the OS lock it was taken with until
    /// it is released.
    pub fn set_process_local(&self, local: bool) {
        self.shared().state().process_local = local;
    }

    /// Whether this filesystem grants a byte-range lock at all, found by
    /// taking and releasing a reserved byte. Any failure counts as no: the
    /// lock-free lease needs none. `true` does not make the locks trusted;
    /// see [`FilesystemInfo::byte_range_locks_qualified`].
    pub fn probe(&self) -> bool {
        let file = self.file();
        // A read-only descriptor can only take a shared lock.
        match os::try_lock(file, PROBE, self.writable) {
            Ok(true) => os::unlock(file, PROBE).is_ok(),
            // Another process probing at this moment: the locks work.
            Ok(false) => true,
            Err(_) => false,
        }
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
        let os = holders.os;
        state.held.remove(&byte.0);
        // Released in the registry even when the OS refuses: the handle
        // still drops whatever the OS kept when its last user closes it.
        let result = os.map_or(Ok(()), |descriptor| {
            os::unlock(shared.descriptor(descriptor), byte.0)
        });
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
            writable: self.writable,
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

    /// Hold the byte in `mode` instead, waiting up to `timeout`: how a
    /// writer sharing a database takes its lease byte exclusive for an
    /// operation that needs the file to itself, and shares it again after.
    /// A shared byte cannot be upgraded in place, so it is let go first.
    /// When `mode` is not granted in time, the old mode is taken back,
    /// waiting as long again, and the failure is returned.
    ///
    /// # Errors
    ///
    /// Returns the failure to take `mode`. When the old mode cannot be
    /// taken back either, that failure is returned instead and the guard
    /// holds nothing.
    pub fn change_mode(&mut self, mode: LockMode, timeout: Duration) -> io::Result<()> {
        if mode == self.mode {
            return Ok(());
        }
        self.held = false;
        // Released in this process even when the OS refuses.
        let _ = self.file.release(self.byte, self.mode);
        let (guard, outcome) = match self.file.lock(self.byte, mode, timeout) {
            Ok(guard) => (guard, Ok(())),
            Err(error) => (self.file.lock(self.byte, self.mode, timeout)?, Err(error)),
        };
        // The guard replaced holds nothing, so dropping it releases nothing.
        *self = guard;
        outcome
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
