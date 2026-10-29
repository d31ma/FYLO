//! A `.fxdb` file opened as a FYLO root (ADR 0016 section 11,
//! `docs/fxdb-segment-backend.md`, "Root integration").
//!
//! A root path that names a regular file is a database: one file holding
//! every collection, each in the segment layout. A collection is its
//! directory block, whose static part holds the catalog descriptor a
//! directory root keeps in `.fylo-catalog/`. Its segments, slots and
//! snapshots are a [`ContainerMedium`] over the [`ContainerOpen`] the
//! process keeps of the file, which every collection in it shares, as a
//! hybrid collection directory's medium shares the open of its own file.
//!
//! A reader takes no lock. A writer opens the file to write on its first
//! write, as every container medium does, and claims each collection it
//! writes; the collection's write lock is that claim.
//!
//! An encrypted database (ADR 0016 section 9) is opened with the key a
//! root was given, which unlocks the process's open of the file. A handle
//! opened without it is locked: it lists no collections, and every read
//! of one and every write fails with `EKEY`, even while another handle in
//! the process has unlocked the open. A wrong key is refused at open.

use std::path::Path;
use std::sync::Arc;

use fylo_format::ContainerSuperblock;

use crate::segments::{ContainerMedium, ContainerOpen, TableCache};
use crate::{
    ContainerCipher, ContainerCollection, ContainerError, ContainerFile, DatabaseKey,
    NativeStorageError, NativeStorageErrorCode,
};

/// A handle on one `.fxdb` database this process has open.
#[derive(Debug)]
pub(crate) struct ContainerDatabase {
    file: Arc<ContainerOpen>,
    /// Whether the file is encrypted and this handle was opened without
    /// its key.
    locked: bool,
}

/// This process's claim on one collection, held while it is written: the
/// collection's medium, which keeps the claim while it is open.
#[derive(Debug)]
pub(crate) struct ContainerWriterLock {
    _medium: ContainerMedium,
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
            if short || error.code() == "EFXDB_NOT_FXDB" {
                NativeStorageError::new(
                    NativeStorageErrorCode::WrongType,
                    "FYLO root is neither a directory nor a .fxdb database",
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
                "the .fxdb database is encrypted and was opened without its key \
                 (fylo.encryption.key)",
            ));
        }
        Ok(())
    }

    fn wrong_key() -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::KeyRequired,
            "the encryption key does not match this .fxdb database (fylo.encryption.key)",
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
        medium.claim()?;
        Ok(ContainerWriterLock { _medium: medium })
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
    /// give its extents back and checkpoint the map, then forget what this
    /// process read of it.
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
                    "the collection was dropped from its .fxdb database",
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
