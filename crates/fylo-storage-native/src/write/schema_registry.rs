//! The schema registry of a directory root (ADR 0017 section 3).
//!
//! Each version of a collection's schema is one immutable file,
//! `<root>/.fylo-schemas/<collection>/<ordinal>.json`, and the head ordinal is
//! the collection descriptor's `schemaHead`. Storage keeps the files, the head
//! and the fence; what a version holds is the engine's business, so its bytes
//! pass through unread.
//!
//! A version is committed by writing its file durably and then changing the
//! descriptor, the way `reshard` changes it, under the collection lock. A file
//! above the head is therefore one a crash left before its descriptor change:
//! it was never committed, nothing reads it, and the next push replaces it.

use fylo_vfs as fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{
    GenerationRecord, NativeWriteRoot, durable_replace, failpoint, json_error, read_bounded_json,
    remove_dir_durable, sync_parent, write_generation,
};
use crate::{
    CollectionKind, MAX_DESCRIPTOR_BYTES, NativeCollection, NativeRoot, NativeStorageError,
    NativeStorageErrorCode, path_exists_no_follow, validate_collection_name,
};

/// Directory beside `.fylo-catalog` that holds every collection's registry.
const REGISTRY_DIRECTORY: &str = ".fylo-schemas";
/// Record of the one-time import from `fylo.schema`. Collection names never
/// contain a dot, so it cannot collide with a collection's directory.
const IMPORT_RECORD: &str = "import.json";
/// Largest schema version file a reader accepts.
pub const MAX_SCHEMA_VERSION_BYTES: u64 = 16 * 1024 * 1024;

/// The current UTC time as `Date.prototype.toISOString` writes it, for the
/// `committed` stamp of a schema version.
///
/// # Errors
///
/// Returns an error when the system clock is before the Unix epoch.
pub fn utc_timestamp() -> Result<String, NativeStorageError> {
    Ok(super::version::iso8601_millis(super::unix_millis()?))
}

impl NativeRoot {
    /// Read one committed schema version, exactly as it was written.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_NOT_FOUND` for an ordinal above the collection's head,
    /// and an error for an invalid name, a linked or oversized file, or I/O.
    pub fn read_schema_version(
        &self,
        collection: &str,
        ordinal: u32,
    ) -> Result<Vec<u8>, NativeStorageError> {
        let head = self.schema_registry_head(collection)?.unwrap_or(0);
        if ordinal == 0 || ordinal > head {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::NotFound,
                format!("schema version {ordinal} of collection {collection} is not committed"),
            ));
        }
        let directory = self.schema_registry_directory();
        Self::require_registry_directory(&directory)?;
        let collection_directory = directory.join(collection);
        Self::require_registry_directory(&collection_directory)?;
        Self::read_registry_file(
            &collection_directory.join(format!("{ordinal}.json")),
            MAX_SCHEMA_VERSION_BYTES,
        )
    }

    /// The import record written the first time this binary imported the
    /// `fylo.schema` folder, or `None` if it never has.
    ///
    /// # Errors
    ///
    /// Returns an error for a linked or oversized record, or I/O.
    pub fn schema_import_record(&self) -> Result<Option<Vec<u8>>, NativeStorageError> {
        self.assert_profile_owned()?;
        let directory = self.schema_registry_directory();
        let record = directory.join(IMPORT_RECORD);
        if !path_exists_no_follow(&directory)? || !path_exists_no_follow(&record)? {
            return Ok(None);
        }
        Self::require_registry_directory(&directory)?;
        Self::read_registry_file(&record, MAX_DESCRIPTOR_BYTES).map(Some)
    }

    pub(crate) fn schema_registry_directory(&self) -> PathBuf {
        self.catalog_root.join(REGISTRY_DIRECTORY)
    }

    fn require_registry_directory(path: &Path) -> Result<(), NativeStorageError> {
        let metadata = fs::symlink_metadata(path).map_err(NativeStorageError::io)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "schema registry path is not a directory",
            ));
        }
        Ok(())
    }

    fn read_registry_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, NativeStorageError> {
        let metadata = fs::symlink_metadata(path).map_err(NativeStorageError::io)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "schema registry file is not a regular file",
            ));
        }
        if metadata.len() > max_bytes {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "schema registry file exceeds its size limit",
            ));
        }
        fs::read(path).map_err(NativeStorageError::io)
    }
}

impl NativeWriteRoot {
    /// The read-only view of this root, for reading what this writer wrote.
    #[must_use]
    pub fn native_root(&self) -> &NativeRoot {
        &self.root
    }

    /// Commit `version` as schema version `ordinal` of a collection.
    ///
    /// Under the collection lock, after recovery, `verify` runs against the
    /// collection as it is (a push dry-runs its migration there), then the
    /// version file is written durably, the generation state takes the
    /// registry fence, and last the descriptor's head moves to `ordinal`. A
    /// crash at any point before the descriptor change leaves the previous
    /// head in force.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_CONCURRENT_WRITE` when `ordinal` does not follow the
    /// head, whatever `verify` returns, and an error for an unsafe path or an
    /// interrupted durable write.
    pub fn commit_schema_version(
        &self,
        collection_name: &str,
        ordinal: u32,
        version: &[u8],
        verify: impl FnOnce(&NativeCollection) -> Result<(), NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        self.root.assert_profile_owned()?;
        let collection = self.root.collection(collection_name)?;
        let _lock = self.lock_collection(&collection.path)?;
        self.recover_locked(&collection)?;
        // Read again under the lock: another push may have moved the head.
        let collection = self.root.collection(collection_name)?;
        let expected = collection.schema_head().unwrap_or(0).saturating_add(1);
        if ordinal != expected {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::ConcurrentWrite,
                format!(
                    "schema registry of {collection_name} moved: version {ordinal} no longer follows its head"
                ),
            ));
        }
        verify(&collection)?;
        let path = self.schema_version_path(collection_name, ordinal)?;
        durable_replace(&path, version)?;
        failpoint("after-schema-file")?;
        self.fence_generation(&collection)?;
        self.write_schema_head(&collection, ordinal)
    }

    /// Record that the one-time import from `fylo.schema` ran.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe path or an interrupted durable write.
    pub fn write_schema_import_record(&self, record: &[u8]) -> Result<(), NativeStorageError> {
        self.root.assert_profile_owned()?;
        let directory = self.registry_directory_created()?;
        durable_replace(&directory.join(IMPORT_RECORD), record)
    }

    /// Remove a dropped collection's registry, so that a collection created
    /// later under the same name starts with none.
    pub(super) fn remove_schema_registry(
        &self,
        collection_name: &str,
    ) -> Result<(), NativeStorageError> {
        let directory = self.root.schema_registry_directory().join(collection_name);
        if path_exists_no_follow(&directory)? {
            remove_dir_durable(&directory)?;
        }
        Ok(())
    }

    fn schema_version_path(
        &self,
        collection_name: &str,
        ordinal: u32,
    ) -> Result<PathBuf, NativeStorageError> {
        validate_collection_name(collection_name)?;
        let directory =
            Self::created_directory(&self.registry_directory_created()?.join(collection_name))?;
        Ok(directory.join(format!("{ordinal}.json")))
    }

    fn registry_directory_created(&self) -> Result<PathBuf, NativeStorageError> {
        Self::created_directory(&self.root.schema_registry_directory())
    }

    /// `path` as a real directory, created durably if it is missing.
    fn created_directory(path: &Path) -> Result<PathBuf, NativeStorageError> {
        if !path_exists_no_follow(path)? {
            fs::create_dir(path).map_err(NativeStorageError::io)?;
            sync_parent(path)?;
        }
        NativeRoot::require_registry_directory(path)?;
        Ok(path.to_owned())
    }

    /// Rewrite `state.json` in the fenced format. Recovery has just left it
    /// stable with an empty log, so only its format changes.
    ///
    /// A segment collection keeps its generation in slots, and its fixed
    /// marker and `docs` file already fence older releases (ADR 0015).
    fn fence_generation(&self, collection: &NativeCollection) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return Ok(());
        }
        let stored = collection.stored_generation()?;
        if stored.schema_fenced() {
            return Ok(());
        }
        write_generation(
            self,
            collection,
            &GenerationRecord::stable(stored.generation).fenced(true),
        )
    }

    fn write_schema_head(
        &self,
        collection: &NativeCollection,
        ordinal: u32,
    ) -> Result<(), NativeStorageError> {
        let path = self
            .root
            .catalog_root()
            .join(".fylo-catalog")
            .join("collections")
            .join(format!("{}.json", collection.name()));
        let mut descriptor: Map<String, Value> = if path_exists_no_follow(&path)? {
            read_bounded_json(&path, MAX_DESCRIPTOR_BYTES)?
        } else {
            // A root older than collection descriptors has none; the defaults
            // `collection` assumed for it become explicit.
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(NativeStorageError::io)?;
            }
            let mut descriptor = Map::new();
            descriptor.insert("version".into(), Value::from(1));
            descriptor.insert(
                "kind".into(),
                Value::from(match collection.kind() {
                    CollectionKind::Document => "document",
                    CollectionKind::File => "file",
                }),
            );
            descriptor
        };
        descriptor.insert("schemas".into(), Value::from("registry"));
        descriptor.insert("schemaHead".into(), Value::from(ordinal));
        let mut encoded = serde_json::to_vec(&descriptor).map_err(|error| json_error(&error))?;
        encoded.push(b'\n');
        durable_replace(&path, &encoded)
    }
}
