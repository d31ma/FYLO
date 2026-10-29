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

use std::path::Path;
use std::sync::Arc;

use fylo_format::ContainerSuperblock;

use crate::segments::{ContainerMedium, ContainerOpen, TableCache};
use crate::{
    ContainerCollection, ContainerError, ContainerFile, NativeStorageError, NativeStorageErrorCode,
};

/// A handle on one `.fxdb` database this process has open.
#[derive(Debug)]
pub(crate) struct ContainerDatabase {
    file: Arc<ContainerOpen>,
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
    /// unless this process already has it open.
    pub(crate) fn open(path: &Path) -> Result<Arc<Self>, NativeStorageError> {
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
        if file.lock().is_encrypted() {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::Unsupported,
                "an encrypted .fxdb database needs its key, which a root does not take yet",
            ));
        }
        Ok(Arc::new(Self { file }))
    }

    /// Create an empty database at `path`, which must not exist, and make
    /// its name durable. What this process kept of a file of the same
    /// name, since removed, is forgotten.
    pub(crate) fn create(path: &Path) -> Result<(), NativeStorageError> {
        ContainerFile::create(path, Self::EXTENT_SHIFT).map_err(ContainerMedium::error)?;
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

    /// Claim the collection in `block` for this process's writes, waiting
    /// for a writer in another process that holds it.
    pub(crate) fn lock_writer(
        &self,
        block: u32,
    ) -> Result<ContainerWriterLock, NativeStorageError> {
        let medium = self.medium(block)?;
        medium.claim()?;
        Ok(ContainerWriterLock { _medium: medium })
    }

    /// The live collection named `name`, reading the directory again when
    /// this open does not know it, as after another process added it.
    pub(crate) fn collection(&self, name: &str) -> Option<ContainerCollection> {
        self.find(|collection| collection.header.name == name)
    }

    /// The names of the live collections, in block order.
    pub(crate) fn collection_names(&self) -> Vec<String> {
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
    /// when this open knows none.
    fn find(&self, matches: impl Fn(&ContainerCollection) -> bool) -> Option<ContainerCollection> {
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
