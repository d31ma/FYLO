//! A `.vdbx` file opened as a FYLO root (ADR 0016 section 11,
//! `docs/vdbx-segment-backend.md`, "Root integration").
//!
//! A root path that names a regular file is a database: one file holding
//! every collection, each in the segment layout. A collection is its
//! directory block, whose static part holds the catalog descriptor a
//! directory root keeps in `.fylo-catalog/`. Its segments, slots and
//! snapshots are a [`ContainerMedium`] over the [`ContainerOpen`] the
//! process keeps of the file, which every collection in it shares, as a
//! hybrid collection directory's medium shares the open of its own file.
//!
//! A reader takes no lock. One that finds a collection published as
//! `writing` by a writer that stopped finishes the write, where it can do
//! so without waiting, or reads past it
//! ([`NativeCollection::database_reader_generation`]). A writer opens the
//! file to write on its first write, as every container medium does, and
//! claims each collection it writes; the collection's write lock is that
//! claim. A compaction of a
//! collection holds its compactor byte, so it can copy without the claim,
//! and a shrink holds the file as its only writer process.
//!
//! An encrypted database (ADR 0016 section 9) is opened with the key a
//! root was given, which unlocks the process's open of the file. A handle
//! opened without it is locked: it lists no collections, and every read
//! of one and every write fails with `EKEY`, even while another handle in
//! the process has unlocked the open. A wrong key is refused at open.

use std::path::Path;
use std::sync::Arc;

use fylo_format::ContainerSuperblock;

use crate::container_allocator::ContainerTailExtent;
use crate::segments::{ContainerMedium, ContainerOpen, SlotWriter, TableCache};
use crate::{
    ContainerCipher, ContainerCollection, ContainerError, ContainerFile, DatabaseKey,
    GenerationState, GenerationStatus, NativeCollection, NativeStorageError,
    NativeStorageErrorCode,
};

/// A handle on one `.vdbx` database this process has open.
#[derive(Debug)]
pub(crate) struct ContainerDatabase {
    file: Arc<ContainerOpen>,
    /// Whether the file is encrypted and this handle was opened without
    /// its key.
    locked: bool,
}

/// This process's claim on one collection, held while it is written: the
/// collection's medium, with a write begun on it. Where writers share the
/// file, the claim lets the collection's writer byte go when the last
/// write in the process ends ([`ContainerMedium::end_write`]).
#[derive(Debug)]
pub(crate) struct ContainerWriterLock {
    medium: ContainerMedium,
}

impl Drop for ContainerWriterLock {
    fn drop(&mut self) {
        self.medium.end_write();
    }
}

/// One compaction's hold on a collection, from its preparation to its
/// adoption: the collection's compactor byte, in place of a directory
/// collection's `locks/compaction.lock`. The copy runs under it alone, so
/// writers of the collection carry on meanwhile.
#[derive(Debug)]
pub(crate) struct ContainerCompactorLock {
    #[cfg(not(target_family = "wasm"))]
    _byte: crate::ByteLockGuard,
}

impl ContainerDatabase {
    /// The extent size a database gets: ADR 0016's 8 MiB.
    const EXTENT_SHIFT: u8 = ContainerSuperblock::DEFAULT_EXTENT_SHIFT;

    /// The database at `path`, which names a regular file, opened to read
    /// unless this process already has it open. An encrypted one is
    /// unlocked with `key`, or the handle is locked without it.
    pub(crate) fn open(
        path: &Path,
        key: Option<&DatabaseKey>,
    ) -> Result<Arc<Self>, NativeStorageError> {
        let file = ContainerOpen::open(path).map_err(|error| {
            // A file too short for the two superblock copies is no database
            // either.
            let short = matches!(&error, ContainerError::Io(error)
                if error.kind() == std::io::ErrorKind::UnexpectedEof);
            if short || error.code() == "EVDBX_NOT_VDBX" {
                NativeStorageError::new(
                    NativeStorageErrorCode::WrongType,
                    "FYLO root is neither a directory nor a .vdbx database",
                )
            } else {
                ContainerMedium::error(error)
            }
        })?;
        let locked = {
            let mut open = file.lock();
            match key {
                _ if !open.is_encrypted() => false,
                None => true,
                Some(key) => {
                    let superblock = open.superblock();
                    let cipher = ContainerCipher::open_file(
                        key.secret(),
                        &superblock.key_salt,
                        &superblock.key_check,
                    )
                    .map_err(|_| Self::wrong_key())?;
                    open.unlock(&Arc::new(cipher))
                        .map_err(ContainerMedium::error)?;
                    false
                }
            }
        };
        Ok(Arc::new(Self { file, locked }))
    }

    /// Create an empty database at `path`, which must not exist, and make
    /// its name durable. With `key` it is encrypted (ADR 0016 section 9);
    /// without, plain. What this process kept of a file of the same name,
    /// since removed, is forgotten.
    pub(crate) fn create(path: &Path, key: Option<&DatabaseKey>) -> Result<(), NativeStorageError> {
        match key {
            Some(key) => ContainerFile::create_encrypted(path, Self::EXTENT_SHIFT, key.secret()),
            None => ContainerFile::create(path, Self::EXTENT_SHIFT),
        }
        .map_err(ContainerMedium::error)?;
        if let Ok(canonical) = fylo_vfs::canonicalize(path) {
            Self::close(&canonical);
        }
        match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => crate::write::sync_directory(parent),
            _ => Ok(()),
        }
    }

    /// Forget this process's open of the database at `path` and what it
    /// read, so the next open reads the file afresh. A writer first
    /// checkpoints the seals its map holds. Handles opened before keep the
    /// old open, and its right to write, until they drop.
    pub(crate) fn close(path: &Path) {
        let file = ContainerOpen::registered(path);
        ContainerOpen::forget(path);
        let Some(file) = file else {
            return;
        };
        let mut open = file.lock();
        for collection in open.collections() {
            let identity = ContainerMedium::identity_of(path, collection);
            TableCache::shared().forget_identity(&identity);
            ContainerMedium::forget_index(&identity);
        }
        if open.is_writable() && open.allocator().has_unsaved_changes() {
            // A failure leaves the previous map, which the next writer
            // settles.
            let _ = open.change_map(|_| Ok(()));
        }
    }

    /// Whether the database is encrypted.
    pub(crate) fn is_encrypted(&self) -> bool {
        self.file.lock().is_encrypted()
    }

    /// Whether the database is a sealed pack, which is never written.
    pub(crate) fn is_sealed(&self) -> bool {
        self.file.lock().is_sealed()
    }

    /// Whether the database is encrypted and this handle has no key for it.
    pub(crate) const fn is_locked(&self) -> bool {
        self.locked
    }

    /// Refuse what a handle without the key may not do: read a collection,
    /// or write.
    pub(crate) fn require_key(&self) -> Result<(), NativeStorageError> {
        if self.locked {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::KeyRequired,
                "the .vdbx database is encrypted and was opened without its key \
                 (fylo.encryption.key)",
            ));
        }
        Ok(())
    }

    fn wrong_key() -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::KeyRequired,
            "the encryption key does not match this .vdbx database (fylo.encryption.key)",
        )
    }

    /// Claim the collection in `block` for this process's writes, waiting
    /// for a writer in another process that holds it.
    pub(crate) fn lock_writer(
        &self,
        block: u32,
    ) -> Result<ContainerWriterLock, NativeStorageError> {
        self.require_key()?;
        let medium = self.medium(block)?;
        medium.begin_write()?;
        Ok(ContainerWriterLock { medium })
    }

    /// Who is behind the `writing` slot of the collection in `block`: a
    /// write this process has begun on it, or else what the lock bytes
    /// tell of other processes ([`ContainerOpen::slot_writer`]). In
    /// WebAssembly the host coordinates writers, and a reader takes every
    /// writer for a running one.
    pub(crate) fn slot_writer(&self, block: u32) -> SlotWriter {
        let Ok(medium) = self.medium(block) else {
            return SlotWriter::Running;
        };
        if medium.is_written() {
            return SlotWriter::Running;
        }
        #[cfg(not(target_family = "wasm"))]
        {
            self.file.slot_writer(block)
        }
        #[cfg(target_family = "wasm")]
        {
            SlotWriter::Running
        }
    }

    /// Take the compactor byte of the collection in `block`, waiting up to
    /// the busy timeout for a compaction in another process or thread.
    pub(crate) fn lock_compactor(
        &self,
        block: u32,
    ) -> Result<ContainerCompactorLock, NativeStorageError> {
        self.require_key()?;
        #[cfg(not(target_family = "wasm"))]
        {
            let byte = self
                .file
                .lock_byte(crate::LockByte::collection_compactor(block))?;
            Ok(ContainerCompactorLock { _byte: byte })
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = block;
            Ok(ContainerCompactorLock {})
        }
    }

    /// Run `run` as the only process writing the file
    /// ([`ContainerOpen::exclusively`]).
    pub(crate) fn exclusively<T>(
        &self,
        run: impl FnOnce() -> Result<T, NativeStorageError>,
    ) -> Result<T, NativeStorageError> {
        self.require_key()?;
        self.file.exclusively(run)
    }

    /// The last extent in use with a free one before it, as the map on
    /// disk shows it: what a shrink moves next.
    pub(crate) fn movable_tail(&self) -> Result<Option<ContainerTailExtent>, NativeStorageError> {
        let mut open = self.file.writable()?;
        open.refresh()?;
        Ok(open.allocator().movable_tail())
    }

    /// The live collection with id `id`, by name and directory block.
    pub(crate) fn collection_with_id(&self, id: u32) -> Option<ContainerCollection> {
        self.find(|collection| collection.header.id == id)
    }

    /// Checkpoint the map, which writes it into the first free extents.
    pub(crate) fn checkpoint_map(&self) -> Result<(), NativeStorageError> {
        self.file.checkpoint_map()
    }

    /// Cut the free extents at the end of the file off, and return the
    /// bytes cut.
    pub(crate) fn cut_free_tail(&self) -> Result<u64, NativeStorageError> {
        self.file.cut_free_tail()
    }

    /// Punch holes over the free extents where the filesystem can, and
    /// return how many it punched.
    pub(crate) fn punch_free_extents(&self) -> Result<u64, NativeStorageError> {
        self.file
            .change_map(|open| open.punch_free_extents().map_err(ContainerMedium::error))
    }

    /// The file's length.
    pub(crate) fn file_bytes(&self) -> Result<u64, NativeStorageError> {
        fylo_vfs::metadata(self.file.path())
            .map(|metadata| metadata.len())
            .map_err(NativeStorageError::io)
    }

    /// The live collection named `name`, reading the directory again when
    /// this open does not know it, as after another process added it.
    pub(crate) fn collection(&self, name: &str) -> Option<ContainerCollection> {
        self.find(|collection| collection.header.name == name)
    }

    /// The names of the live collections, in block order; none for a
    /// handle without the key.
    pub(crate) fn collection_names(&self) -> Vec<String> {
        if self.locked {
            return Vec::new();
        }
        let mut open = self.file.lock();
        // A failure keeps the directory this open read.
        let _ = open.refresh();
        open.collections()
            .iter()
            .filter(|collection| !collection.header.dropped)
            .map(|collection| collection.header.name.clone())
            .collect()
    }

    /// Add a collection with catalog descriptor `descriptor`, and
    /// checkpoint the map, so other processes reading it find the
    /// collection. Returns `false` when a live collection has the name, as
    /// when another process added it first.
    pub(crate) fn add_collection(
        &self,
        name: &str,
        descriptor: &[u8],
    ) -> Result<bool, NativeStorageError> {
        self.require_key()?;
        self.file
            .change_map(|open| match open.add_collection(name, descriptor) {
                Ok(_) => open.checkpoint().map(|()| true),
                Err(ContainerError::CollectionExists) => Ok(false),
                Err(error) => Err(ContainerMedium::error(error)),
            })
    }

    /// Drop the collection named `name`, which this process has claimed,
    /// and holds the compactor byte of, so no compaction's copy is given
    /// back while it is written: give its extents back and checkpoint the
    /// map, then forget what this process read of it.
    pub(crate) fn drop_collection(&self, name: &str) -> Result<(), NativeStorageError> {
        self.require_key()?;
        let collection = self.collection(name).ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::NotFound,
                format!("collection \"{name}\" does not exist"),
            )
        })?;
        self.file.change_map(|open| {
            open.drop_collection(name).map_err(ContainerMedium::error)?;
            open.forget_table(collection.header.id);
            open.checkpoint()
        })?;
        let identity = ContainerMedium::identity_of(self.file.path(), &collection);
        TableCache::shared().forget_identity(&identity);
        ContainerMedium::forget_index(&identity);
        Ok(())
    }

    /// The medium of the collection in directory block `block`.
    pub(crate) fn medium(&self, block: u32) -> Result<ContainerMedium, NativeStorageError> {
        self.require_key()?;
        let collection = self
            .find(|collection| collection.block == block)
            .ok_or_else(|| {
                NativeStorageError::new(
                    NativeStorageErrorCode::NotFound,
                    "the collection was dropped from its .vdbx database",
                )
            })?;
        Ok(ContainerMedium::attach(&self.file, &collection))
    }

    /// The live collection `matches` picks, reading the directory again
    /// when this open knows none; none for a handle without the key.
    fn find(&self, matches: impl Fn(&ContainerCollection) -> bool) -> Option<ContainerCollection> {
        if self.locked {
            return None;
        }
        let live = |collections: &[ContainerCollection]| {
            collections
                .iter()
                .find(|collection| !collection.header.dropped && matches(collection))
                .cloned()
        };
        let mut open = self.file.lock();
        if let Some(collection) = live(open.collections()) {
            return Some(collection);
        }
        if open.refresh().unwrap_or(false) {
            return live(open.collections());
        }
        None
    }
}

impl NativeCollection {
    /// The generation a reader of a database collection gets while its
    /// slot says `writing`, as `stored` does. Readers are never blocked
    /// (ADR 0016 section 7).
    ///
    /// A writer that is still running publishes in a moment, so the reader
    /// is told it is writing and retries, as in a directory root. One that
    /// stopped left its write to the next writer of the collection, and a
    /// reader finishes it first, as a directory root's reader recovers a
    /// stranded write, where it may write the file without waiting or
    /// keeping another writer out ([`SlotWriter::Stopped`]). Otherwise, or
    /// when finishing fails, as on a full disk, it reads the collection as
    /// the slot last published it. That is whole: the records up to the
    /// position a `writing` slot names stay as they are, a checkpoint
    /// names new snapshot runs only once they reach that position, and a
    /// compaction releases the segments it replaced only after a slot
    /// names its record.
    pub(crate) fn database_reader_generation(
        &self,
        stored: GenerationState,
    ) -> Result<GenerationState, NativeStorageError> {
        let (Some(database), Some(block)) = (self.root.database(), self.block) else {
            return Ok(stored);
        };
        match database.slot_writer(block) {
            SlotWriter::Running => return Ok(stored),
            SlotWriter::Stopped => {
                // A failure leaves the write to the next writer.
                #[cfg(not(target_family = "wasm"))]
                let _ = crate::NativeWriteRoot::finish_stranded(&self.root, &self.name);
            }
            SlotWriter::Unsettled => {}
        }
        let state = self.stored_generation()?;
        if state.state == GenerationStatus::Stable {
            return Ok(state);
        }
        Ok(GenerationState {
            state: GenerationStatus::Stable,
            transaction_id: None,
            ..state
        })
    }
}
