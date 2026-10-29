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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
#[cfg(not(target_family = "wasm"))]
use std::time::{Duration, Instant};

use super::{ContainerMedium, SEGMENT};
use crate::{ContainerError, ContainerFile, NativeStorageError};
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
            if let Ok(file) = ContainerFile::open_reader(&self.path) {
                open.file = file;
                open.access = None;
                open.forget_tables();
            }
            return Err(ContainerMedium::error(error));
        }
        Ok(open)
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

    /// Reopen the file to write: natively as ADR 0016 section 7
    /// coordinates writers; in WebAssembly without coordination, which the
    /// host provides. Extents a dropped collection still owns, which a
    /// writer that stopped after dropping it left, are given back.
    fn upgrade(&mut self, path: &Path) -> Result<(), NativeStorageError> {
        #[cfg(not(target_family = "wasm"))]
        {
            let options = ContainerOpen::write_options();
            let (file, access) =
                ContainerFile::open_writer(path, options).map_err(ContainerMedium::error)?;
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
            self.file = ContainerFile::open_unchecked(path).map_err(ContainerMedium::error)?;
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
