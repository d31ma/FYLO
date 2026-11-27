use byckr_engine::CollectionInspection;
use byckr_storage_native::NativeStorageErrorCode;

use crate::{Error, Root};

/// The collections of an open root (ADR 0035 section 1).
#[derive(Clone, Copy, Debug)]
pub struct Collections<'root> {
    root: &'root Root,
}

impl<'root> Collections<'root> {
    pub(crate) const fn new(root: &'root Root) -> Self {
        Self { root }
    }

    /// Whether the collection `name` exists.
    ///
    /// # Errors
    ///
    /// A stable error for an invalid name or unreadable storage.
    pub fn exists(&self, name: &str) -> Result<bool, Error> {
        match self.root.reader()?.collection_kind(name) {
            Ok(_) => Ok(true),
            Err(error) if error.storage_code() == Some(NativeStorageErrorCode::NotFound) => {
                Ok(false)
            }
            Err(error) => Err(Error::from(error)),
        }
    }

    /// The names of the root's collections, sorted. It reads only the
    /// catalog and the collection folders, never a document.
    ///
    /// # Errors
    ///
    /// A stable error for a corrupt descriptor or unreadable storage.
    pub fn list(&self) -> Result<Vec<String>, Error> {
        Ok(self.root.reader()?.collection_names()?)
    }

    /// Create the document collection `name`; answers `false` when it
    /// already existed.
    ///
    /// # Errors
    ///
    /// A stable error for an invalid name or a failed write.
    pub fn create(&self, name: &str) -> Result<bool, Error> {
        self.root.create_collection(name)
    }

    /// Drop the collection `name`.
    ///
    /// # Errors
    ///
    /// `ENATIVE_NOT_FOUND` for a missing collection, or a failed write.
    pub fn drop(&self, name: &str) -> Result<(), Error> {
        self.root.drop_collection(name)
    }

    /// Counts and index state of the collection `name`.
    ///
    /// # Errors
    ///
    /// `ENATIVE_NOT_FOUND` for a missing collection.
    pub fn inspect(&self, name: &str) -> Result<CollectionInspection, Error> {
        self.root.inspect_collection(name)
    }
}
