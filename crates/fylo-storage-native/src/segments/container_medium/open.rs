//! One `.fxdb` file open in this process (`docs/fxdb-segment-backend.md`,
//! "Locks"), shared by every collection medium over it: the one collection
//! of a hybrid collection directory's `segments.fxdb`, or every collection
//! of a `.fxdb` database root.
//!
//! The file opens to read and takes no lock. The first write reopens it to
//! write through [`ContainerFile::open_writer`], which coordinates with
//! writers in other processes by byte-range locks where they are qualified
//! (the lease byte held shared) and by the single-writer lease elsewhere.
//! Every change to the extent map is a [`Open::change_map`]: under the
//! allocation byte, the map is read afresh when another process checkpointed
//! it, changed, and checkpointed before the byte is let go, so writers in
//! several processes never hand out the same extent.
//!
//! An encrypted file opens locked. The first handle that brings its key
//! unlocks the open, which keeps the derived key and unlocks every file it
//! opens afresh with it: a writer's reopen, and a reader's after a lost
//! lease. A handle without the key keeps away from the collections itself
//! (`ContainerDatabase`), so one handle's key never serves another.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
#[cfg(not(target_family = "wasm"))]
use std::time::{Duration, Instant};

use super::{ContainerMedium, SEGMENT};
use crate::{ContainerCipher, ContainerError, ContainerFile, NativeStorageError};
#[cfg(not(target_family = "wasm"))]
use crate::{ContainerWriteAccess, ContainerWriteOptions};

/// One `.fxdb` file this process has open. The registry keeps none alive:
/// the file closes when the last medium and the last database handle on it
/// drop, and is then opened afresh.
#[derive(Debug)]
pub(crate) struct ContainerOpen {
    path: PathBuf,
    open: Mutex<Open>,
}

/// The open file and what this process keeps about it.
#[derive(Debug)]
pub(crate) struct Open {
    pub(super) file: ContainerFile,
    /// Segment extents by sequence number, which is the segment number, per
    /// collection id. A collection's table is built from the map the first
    /// time it is used and after a refresh, and kept as this process takes,
    /// takes back and releases extents, so a lookup never scans the map.
    pub(super) segments: BTreeMap<u32, BTreeMap<u64, u64>>,
    /// Segments sealed since the last checkpoint, which a refresh that
    /// reads another process's map seals again.
    seals: Vec<Seal>,
    /// How many times this open became a writer. A claim on a collection
    /// holds for the writer it was made under ([`Self::writer_epoch`]).
    epoch: u64,
    /// The key an encrypted file was unlocked with, for every file this
    /// open reads afresh.
    cipher: Option<Arc<ContainerCipher>>,
    /// What lets this process write, once it has written.
    #[cfg(not(target_family = "wasm"))]
    pub(super) access: Option<WriteRight>,
}

/// A seal the map on disk does not show yet.
#[derive(Clone, Copy, Debug)]
struct Seal {
    extent: u64,
    owner: u32,
    sequence: u64,
    live_bytes: u64,
}

/// A writer's hold on the file.
#[cfg(not(target_family = "wasm"))]
#[derive(Debug)]
pub(crate) struct WriteRight {
    pub(super) access: ContainerWriteAccess,
    renewed: Instant,
    renew_every: Duration,
    pub(super) busy_timeout: Duration,
}

#[cfg(not(target_family = "wasm"))]
impl WriteRight {
    /// Check that the lease, if there is one, is still this process's, and
    /// renew it once a third of its takeover time has passed since the last
    /// renewal.
    fn check(&mut self) -> Result<(), ContainerError> {
        self.access.assert_writable()?;
        if self.renewed.elapsed() >= self.renew_every {
            self.access.renew()?;
            self.renewed = Instant::now();
        }
        Ok(())
    }

    /// Whether writers of other processes share the file, each holding the
    /// writer byte of the collections it writes.
    pub(super) fn shares_the_file(&self) -> bool {
        matches!(self.access, ContainerWriteAccess::SharedLocks { .. })
    }
}

impl ContainerOpen {
    /// The open of the file at `path`, opened to read unless this process
    /// already has it open.
    ///
    /// # Errors
    ///
    /// Returns the open or read failure, or a decoding error.
    pub(crate) fn open(path: &Path) -> Result<Arc<Self>, ContainerError> {
        let mut files = Self::registry();
        files.retain(|_, open| open.strong_count() > 0);
        if let Some(open) = files.get(path).and_then(Weak::upgrade) {
            return Ok(open);
        }
        let open = Arc::new(Self {
            path: path.to_owned(),
            open: Mutex::new(Open::new(ContainerFile::open_reader(path)?)),
        });
        files.insert(path.to_owned(), Arc::downgrade(&open));
        Ok(open)
    }

    /// The open of the file at `path`, when this process has one.
    pub(crate) fn registered(path: &Path) -> Option<Arc<Self>> {
        Self::registry().get(path).and_then(Weak::upgrade)
    }

    /// Forget this process's open of the file at `path`, and the media
    /// over it, as a process that stopped would: the next access opens the
    /// file afresh, while handles on the old open keep it.
    pub(crate) fn forget(path: &Path) {
        Self::registry().remove(path);
        ContainerMedium::forget_media(path);
    }

    /// The file's path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Open> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The open, writable and with this process's right to write checked.
    /// A reader's open is replaced by a writer's first.
    ///
    /// # Errors
    ///
    /// Returns `EBUSY` when other writers keep the file, a lost lease, or
    /// the open failure.
    pub(crate) fn writable(&self) -> Result<MutexGuard<'_, Open>, NativeStorageError> {
        let mut open = self.lock();
        if !open.file.is_writable() {
            open.upgrade(&self.path)?;
        }
        #[cfg(not(target_family = "wasm"))]
        if let Some(right) = &mut open.access
            && let Err(error) = right.check()
        {
            // A writer whose lease another process took over reads on;
            // its next write opens the file to write afresh and claims its
            // collections again.
            open.demote(&self.path);
            return Err(ContainerMedium::error(error));
        }
        Ok(open)
    }

    /// Take `byte` exclusive through the writable open's lock handle,
    /// waiting up to the busy timeout. The open is not held while waiting.
    ///
    /// # Errors
    ///
    /// Returns `EBUSY` when another holder keeps it past the busy timeout,
    /// or what [`Self::writable`] returns.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn lock_byte(
        &self,
        byte: crate::LockByte,
    ) -> Result<crate::ByteLockGuard, NativeStorageError> {
        let (handle, wait) = {
            let open = self.writable()?;
            let wait = open.access.as_ref().map_or_else(
                || ContainerOpen::write_options().busy_timeout,
                |right| right.busy_timeout,
            );
            (open.lock_handle().map_err(ContainerMedium::error)?, wait)
        };
        crate::container_file::lock_byte(&handle, byte, wait).map_err(ContainerMedium::error)
    }

    /// Take `byte` exclusive without waiting, or `None` when another holder,
    /// in this process or another, has it.
    ///
    /// # Errors
    ///
    /// Returns the lock failure, or what [`Self::writable`] returns.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn try_lock_byte(
        &self,
        byte: crate::LockByte,
    ) -> Result<Option<crate::ByteLockGuard>, NativeStorageError> {
        let handle = self
            .writable()?
            .lock_handle()
            .map_err(ContainerMedium::error)?;
        handle
            .try_lock(byte, crate::LockMode::Exclusive)
            .map_err(NativeStorageError::io)
    }

    /// Run `run` as the only process writing the file (ADR 0016 section
    /// 7): a writer sharing the file takes the lease byte exclusive for it,
    /// waiting up to the busy timeout for the other writer processes to
    /// close the file, and shares it again after; under the lease this
    /// process is the only writer already. Readers are never kept out.
    /// `run` takes the open itself as it needs it.
    ///
    /// # Errors
    ///
    /// Returns `EBUSY` when other writers keep the file, what `run`
    /// returns, or the failure to share the file again, after which this
    /// open reads on and its next write opens the file afresh.
    pub(crate) fn exclusively<T>(
        &self,
        run: impl FnOnce() -> Result<T, NativeStorageError>,
    ) -> Result<T, NativeStorageError> {
        #[cfg(not(target_family = "wasm"))]
        self.lease_mode(true)?;
        let outcome = run();
        #[cfg(not(target_family = "wasm"))]
        let shared = self.lease_mode(false);
        #[cfg(target_family = "wasm")]
        let shared: Result<(), NativeStorageError> = Ok(());
        let value = outcome?;
        shared?;
        Ok(value)
    }

    /// Hold the file exclusively, or share it again. An open whose lease
    /// byte is lost on the way reads on.
    #[cfg(not(target_family = "wasm"))]
    fn lease_mode(&self, exclusive: bool) -> Result<(), NativeStorageError> {
        let mut open = if exclusive {
            self.writable()?
        } else {
            self.lock()
        };
        let Some(right) = open.access.as_mut() else {
            return Ok(());
        };
        let wait = right.busy_timeout;
        let changed = if exclusive {
            right.access.hold_exclusively(wait)
        } else {
            right.access.share(wait)
        };
        match changed {
            Ok(()) => Ok(()),
            Err(error) => {
                if exclusive && matches!(error, ContainerError::Busy) {
                    // Refused in time, and shared again.
                    return Err(ContainerMedium::error(error));
                }
                open.demote(&self.path);
                Err(ContainerMedium::error(error))
            }
        }
    }

    /// Run `change` as a change to the map ([`Open::change_map`]), opening
    /// the file to write first.
    ///
    /// # Errors
    ///
    /// Returns what [`Self::writable`] or `change` returns, or the lock or
    /// checkpoint failure.
    pub(crate) fn change_map<T>(
        &self,
        change: impl FnOnce(&mut Open) -> Result<T, NativeStorageError>,
    ) -> Result<T, NativeStorageError> {
        self.writable()?.change_map(change)
    }

    /// Checkpoint the map, which writes it into the first free extents.
    ///
    /// # Errors
    ///
    /// Returns what [`Self::change_map`] returns.
    pub(crate) fn checkpoint_map(&self) -> Result<(), NativeStorageError> {
        self.change_map(Open::checkpoint)
    }

    /// Cut the free extents at the end of the file off
    /// ([`Open::cut_free_tail`]), and return the bytes cut.
    ///
    /// # Errors
    ///
    /// Returns what [`Self::change_map`] returns.
    pub(crate) fn cut_free_tail(&self) -> Result<u64, NativeStorageError> {
        self.change_map(Open::cut_free_tail)
    }

    fn registry() -> MutexGuard<'static, BTreeMap<PathBuf, Weak<ContainerOpen>>> {
        static FILES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<ContainerOpen>>>> = OnceLock::new();
        FILES
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// How this process opens a container to write: the defaults, unless a
    /// caller set others.
    #[cfg(not(target_family = "wasm"))]
    fn write_options() -> ContainerWriteOptions {
        *Self::configured_options()
    }

    /// Open containers to write with `options` from now on, in this whole
    /// process: exclusive mode, the busy timeout and the lease timing. Only
    /// tests choose yet; a `.fxdb` root will take them from its options.
    #[cfg(all(test, not(target_family = "wasm")))]
    pub(crate) fn set_write_options(options: ContainerWriteOptions) {
        *Self::configured_options() = options;
    }

    #[cfg(not(target_family = "wasm"))]
    fn configured_options() -> MutexGuard<'static, ContainerWriteOptions> {
        static OPTIONS: OnceLock<Mutex<ContainerWriteOptions>> = OnceLock::new();
        OPTIONS
            .get_or_init(|| Mutex::new(ContainerWriteOptions::default()))
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Open {
    fn new(file: ContainerFile) -> Self {
        Self {
            file,
            segments: BTreeMap::new(),
            seals: Vec::new(),
            epoch: 0,
            cipher: None,
            #[cfg(not(target_family = "wasm"))]
            access: None,
        }
    }

    /// Which writer this open is: it changes each time the open becomes
    /// one, and a claim made under another is made again.
    pub(crate) fn writer_epoch(&self) -> u64 {
        self.epoch
    }

    fn forget_tables(&mut self) {
        self.segments.clear();
        self.seals.clear();
    }

    /// Unlock an encrypted file with `cipher`, a key derived for it and
    /// checked against it, and keep the key for the files this open reads
    /// afresh. Nothing happens for a plain file or one already unlocked.
    ///
    /// # Errors
    ///
    /// Returns `Cipher(WrongKey)` for another file's key, or the failure to
    /// read the directory.
    pub(crate) fn unlock(&mut self, cipher: &Arc<ContainerCipher>) -> Result<(), ContainerError> {
        if !self.file.is_locked() {
            return Ok(());
        }
        self.file.unlock_with(Arc::clone(cipher))?;
        self.cipher = Some(Arc::clone(cipher));
        Ok(())
    }

    /// Give up writing: read on through a reader's open, unlocked with the
    /// key this open keeps, so the next write opens the file to write
    /// afresh and claims its collections again.
    #[cfg(not(target_family = "wasm"))]
    fn demote(&mut self, path: &Path) {
        if let Ok(mut file) = ContainerFile::open_reader(path)
            && self
                .cipher
                .clone()
                .is_none_or(|cipher| file.unlock_with(cipher).is_ok())
        {
            self.file = file;
            self.access = None;
            self.forget_tables();
        }
    }

    /// Reopen the file to write: natively as ADR 0016 section 7
    /// coordinates writers; in WebAssembly without coordination, which the
    /// host provides. Extents a dropped collection still owns, which a
    /// writer that stopped after dropping it left, are given back.
    fn upgrade(&mut self, path: &Path) -> Result<(), NativeStorageError> {
        #[cfg(not(target_family = "wasm"))]
        {
            let options = ContainerOpen::write_options();
            let (mut file, access) =
                ContainerFile::open_writer(path, options).map_err(ContainerMedium::error)?;
            if let Some(cipher) = &self.cipher {
                file.unlock_with(Arc::clone(cipher))
                    .map_err(ContainerMedium::error)?;
            }
            self.file = file;
            self.access = Some(WriteRight {
                access,
                renewed: Instant::now(),
                renew_every: options.lease_timing.takeover_after / 3,
                busy_timeout: options.busy_timeout,
            });
        }
        #[cfg(target_family = "wasm")]
        {
            let mut file = ContainerFile::open_unchecked(path).map_err(ContainerMedium::error)?;
            if let Some(cipher) = &self.cipher {
                file.unlock_with(Arc::clone(cipher))
                    .map_err(ContainerMedium::error)?;
            }
            self.file = file;
        }
        self.forget_tables();
        self.epoch += 1;
        self.change_map(|open| open.file.release_dropped().map_err(ContainerMedium::error))
    }

    /// Change the map as the one process allocating: take the allocation
    /// byte, read the map afresh if another process checkpointed it since,
    /// run `change`, and checkpoint whatever it changed before the byte is
    /// let go, also when `change` fails part way.
    ///
    /// # Errors
    ///
    /// Returns what `change` returns, or the lock, refresh or checkpoint
    /// failure.
    pub(crate) fn change_map<T>(
        &mut self,
        change: impl FnOnce(&mut Self) -> Result<T, NativeStorageError>,
    ) -> Result<T, NativeStorageError> {
        #[cfg(not(target_family = "wasm"))]
        let _allocation = {
            let wait = self.access.as_ref().map_or_else(
                || ContainerOpen::write_options().busy_timeout,
                |right| right.busy_timeout,
            );
            self.file
                .lock_allocation(wait)
                .map_err(ContainerMedium::error)?
        };
        self.refresh()?;
        let changed = change(self);
        let saved = if self.file.allocator().has_unsaved_changes() {
            self.checkpoint()
        } else {
            Ok(())
        };
        let value = changed?;
        saved?;
        Ok(value)
    }

    /// Checkpoint the map, with the seals this process made since.
    pub(crate) fn checkpoint(&mut self) -> Result<(), NativeStorageError> {
        self.file.checkpoint().map_err(ContainerMedium::error)?;
        self.seals.clear();
        Ok(())
    }

    /// Cut the free extents at the end of the file off, as often as a
    /// checkpoint that moves the map frees more there, and return the bytes
    /// cut ([`ContainerFile::shrink_free_tail`]).
    pub(crate) fn cut_free_tail(&mut self) -> Result<u64, NativeStorageError> {
        let mut cut = 0;
        loop {
            let bytes = self
                .file
                .shrink_free_tail()
                .map_err(ContainerMedium::error)?;
            self.seals.clear();
            if bytes == 0 {
                return Ok(cut);
            }
            cut += bytes;
        }
    }

    /// Set read-only-compatible feature bits in the superblock durably,
    /// through a checkpoint that makes this process's seals durable too.
    pub(crate) fn add_read_only_feature(
        &mut self,
        features: u32,
    ) -> Result<(), NativeStorageError> {
        if self
            .file
            .add_read_only_compat(features)
            .map_err(ContainerMedium::error)?
        {
            self.seals.clear();
        }
        Ok(())
    }

    /// Set incompatible feature bits in the superblock durably, through a
    /// checkpoint that makes this process's seals durable too.
    pub(crate) fn add_incompat_feature(&mut self, features: u32) -> Result<(), NativeStorageError> {
        if self
            .file
            .add_incompat(features)
            .map_err(ContainerMedium::error)?
        {
            self.seals.clear();
        }
        Ok(())
    }

    /// Read the map and the directory again if a checkpoint moved them,
    /// rebuilding the segment tables and sealing again what this process
    /// sealed since its last checkpoint. Returns whether it read them.
    pub(crate) fn refresh(&mut self) -> Result<bool, NativeStorageError> {
        if !self.file.refresh().map_err(ContainerMedium::error)? {
            return Ok(false);
        }
        self.segments.clear();
        let file = &mut self.file;
        self.seals.retain(|seal| {
            file.allocator().in_use(seal.extent) == Some((SEGMENT, seal.owner, seal.sequence))
                && file.seal(seal.extent, seal.live_bytes).is_ok()
        });
        Ok(true)
    }

    /// Seal segment `sequence` of collection `owner` in `extent`, in the
    /// map in memory; the next checkpoint makes it durable.
    pub(crate) fn seal(
        &mut self,
        (extent, owner, sequence): (u64, u32, u64),
        live_bytes: u64,
    ) -> Result<(), NativeStorageError> {
        self.file
            .seal(extent, live_bytes)
            .map_err(ContainerMedium::error)?;
        self.seals.push(Seal {
            extent,
            owner,
            sequence,
            live_bytes,
        });
        Ok(())
    }

    /// Drop the segment table of collection `owner`.
    pub(crate) fn forget_table(&mut self, owner: u32) {
        self.segments.remove(&owner);
    }

    /// The segment table of collection `owner`, built from the map when
    /// it has none.
    pub(crate) fn table(&mut self, owner: u32) -> &mut BTreeMap<u64, u64> {
        let file = &self.file;
        self.segments.entry(owner).or_insert_with(|| {
            file.allocator()
                .owned(SEGMENT, owner)
                .into_iter()
                .map(|(extent, number)| (number, extent))
                .collect()
        })
    }
}

impl std::ops::Deref for Open {
    type Target = ContainerFile;

    fn deref(&self) -> &ContainerFile {
        &self.file
    }
}

impl std::ops::DerefMut for Open {
    fn deref_mut(&mut self) -> &mut ContainerFile {
        &mut self.file
    }
}
