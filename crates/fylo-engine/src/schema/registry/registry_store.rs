use std::path::Path;

use fylo_storage_native::{
    NativeRoot, NativeStorageError, NativeStorageErrorCode, NativeWriteRoot,
};

/// Where a database keeps its schema registry (ADR 0017 section 3).
///
/// A directory root on the file layout keeps one file per version and the
/// head in the collection descriptor ([`DirectoryRegistryStore`]). A `.fxdb`
/// file or a segment collection will keep each version as a record of kind 5
/// and a schemas snapshot (extent kind 7) once ADR 0015's segments land; that
/// store implements this trait and nothing above it changes.
pub(crate) trait SchemaRegistryStore {
    /// Identity of the database, which keys the process-wide version cache.
    fn location(&self) -> &Path;

    /// Ordinal of a collection's head, or `None` without a registry.
    fn head(&self, collection: &str) -> Result<Option<u32>, NativeStorageError>;

    /// Bytes of one committed version.
    fn read(&self, collection: &str, ordinal: u32) -> Result<Vec<u8>, NativeStorageError>;

    /// Commit `version` as `ordinal`, which must follow the head. `verify`
    /// runs first, with the collection held against other writers.
    fn commit(
        &self,
        collection: &str,
        ordinal: u32,
        version: &[u8],
        verify: &mut dyn FnMut() -> Result<(), NativeStorageError>,
    ) -> Result<(), NativeStorageError>;

    /// The record of the one-time import from `fylo.schema`, if it ran.
    fn import_record(&self) -> Result<Option<Vec<u8>>, NativeStorageError>;

    fn write_import_record(&self, record: &[u8]) -> Result<(), NativeStorageError>;
}

/// The registry of a directory root: `<root>/.fylo-schemas/<collection>/
/// <ordinal>.json`, with the head in the catalog descriptor.
pub(crate) struct DirectoryRegistryStore {
    root: NativeRoot,
    writer: Option<NativeWriteRoot>,
}

impl DirectoryRegistryStore {
    pub(crate) fn new(root: NativeRoot, writer: Option<NativeWriteRoot>) -> Self {
        Self { root, writer }
    }

    fn writer(&self) -> Result<&NativeWriteRoot, NativeStorageError> {
        self.writer.as_ref().ok_or_else(|| {
            NativeStorageError::codec_rejected(
                "a read-only engine cannot change the schema registry",
            )
        })
    }
}

impl SchemaRegistryStore for DirectoryRegistryStore {
    fn location(&self) -> &Path {
        self.root.catalog_root()
    }

    fn head(&self, collection: &str) -> Result<Option<u32>, NativeStorageError> {
        match self.root.schema_registry_head(collection) {
            // A name storage refuses cannot have a registry; the operation
            // that uses it reports the name in its own words.
            Err(error) if error.code() == NativeStorageErrorCode::InvalidCollection => Ok(None),
            outcome => outcome,
        }
    }

    fn read(&self, collection: &str, ordinal: u32) -> Result<Vec<u8>, NativeStorageError> {
        self.root.read_schema_version(collection, ordinal)
    }

    fn commit(
        &self,
        collection: &str,
        ordinal: u32,
        version: &[u8],
        verify: &mut dyn FnMut() -> Result<(), NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        self.writer()?
            .commit_schema_version(collection, ordinal, version, |_| verify())
    }

    fn import_record(&self) -> Result<Option<Vec<u8>>, NativeStorageError> {
        self.root.schema_import_record()
    }

    fn write_import_record(&self, record: &[u8]) -> Result<(), NativeStorageError> {
        self.writer()?.write_schema_import_record(record)
    }
}
