use std::collections::HashMap;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use fylo_storage_native::{NativeRoot, NativeStorageError, NativeWriteRoot};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::loaded_version::LoadedVersion;
use super::registry_store::{DirectoryRegistryStore, SchemaRegistryStore};
use super::registry_version::RegistryVersion;

/// Versions parsed in this process, by database, collection, ordinal and the
/// digest of the version file. Committed versions never change, and the
/// digest keeps a collection dropped and created again from reusing the
/// versions of its predecessor.
type LoadedVersions = HashMap<(PathBuf, String, u32, String), Arc<LoadedVersion>>;
/// Versions one handle has read, by collection and ordinal.
type HandleVersions = Arc<Mutex<HashMap<(String, u32), Arc<LoadedVersion>>>>;
/// Heads a read engine has seen, by collection.
type RememberedHeads = Arc<Mutex<HashMap<String, Option<u32>>>>;

/// The schema registry of one database, read through its store with every
/// version parsed once (ADR 0017 section 3).
///
/// Collections without a registry are not this type's business: every method
/// answers `None` for them, and the caller falls back to `fylo.schema`.
#[derive(Clone)]
pub(crate) struct SchemaRegistry {
    store: Arc<dyn SchemaRegistryStore + Send + Sync>,
    /// Versions this handle has read, so a scan that decrypts each document
    /// at its own `_v` reads each version file once.
    versions: HandleVersions,
    /// Heads seen by a read engine, which answers every read of one request
    /// from one head. A write engine reads the head on every write instead.
    heads: Option<RememberedHeads>,
}

impl SchemaRegistry {
    /// The registry of a directory root.
    pub(crate) fn directory(root: NativeRoot, writer: Option<NativeWriteRoot>) -> Self {
        Self {
            store: Arc::new(DirectoryRegistryStore::new(root, writer)),
            versions: Arc::default(),
            heads: None,
        }
    }

    /// Remember each collection's head for this handle's lifetime.
    pub(crate) fn with_remembered_heads(mut self) -> Self {
        self.heads = Some(Arc::default());
        self
    }

    pub(crate) fn head_ordinal(&self, collection: &str) -> Result<Option<u32>, String> {
        let Some(heads) = &self.heads else {
            return self
                .store
                .head(collection)
                .map_err(|error| error.to_string());
        };
        if let Some(head) = Self::lock(heads).get(collection) {
            return Ok(*head);
        }
        let head = self
            .store
            .head(collection)
            .map_err(|error| error.to_string())?;
        Self::lock(heads).insert(collection.to_owned(), head);
        Ok(head)
    }

    /// The head version, or `None` for a collection without a registry.
    pub(crate) fn head(&self, collection: &str) -> Result<Option<Arc<LoadedVersion>>, String> {
        self.head_ordinal(collection)?
            .map(|ordinal| self.version(collection, ordinal))
            .transpose()
    }

    /// Every version from ordinal 1 to the head, or none without a registry.
    pub(crate) fn versions(&self, collection: &str) -> Result<Vec<Arc<LoadedVersion>>, String> {
        let head = self.head_ordinal(collection)?.unwrap_or(0);
        (1..=head)
            .map(|ordinal| self.version(collection, ordinal))
            .collect()
    }

    /// The version labelled `label`, if the registry has one.
    pub(crate) fn at_label(
        &self,
        collection: &str,
        label: &str,
    ) -> Result<Option<Arc<LoadedVersion>>, String> {
        Ok(self
            .versions(collection)?
            .into_iter()
            .find(|version| version.label() == label))
    }

    pub(crate) fn version(
        &self,
        collection: &str,
        ordinal: u32,
    ) -> Result<Arc<LoadedVersion>, String> {
        let key = (collection.to_owned(), ordinal);
        if let Some(version) = Self::lock(&self.versions).get(&key) {
            return Ok(Arc::clone(version));
        }
        let bytes = self
            .store
            .read(collection, ordinal)
            .map_err(|error| error.to_string())?;
        let shared = (
            self.store.location().to_owned(),
            collection.to_owned(),
            ordinal,
            Sha256::digest(&bytes)
                .iter()
                .fold(String::with_capacity(64), |mut text, byte| {
                    let _ = write!(text, "{byte:02x}");
                    text
                }),
        );
        let cached = Self::lock(Self::loaded()).get(&shared).cloned();
        let version = if let Some(version) = cached {
            version
        } else {
            let version = Arc::new(LoadedVersion::parse(collection, ordinal, &bytes)?);
            Self::lock(Self::loaded()).insert(shared, Arc::clone(&version));
            version
        };
        Self::lock(&self.versions).insert(key, Arc::clone(&version));
        Ok(version)
    }

    /// The document migrated to the head, or `None` without a registry.
    pub(crate) fn materialize(
        &self,
        collection: &str,
        document: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, String> {
        let versions = self.versions(collection)?;
        if versions.is_empty() {
            return Ok(None);
        }
        Self::materialize_through(collection, &versions, document).map(Some)
    }

    /// Validate a document against the head and stamp `_v` with its label,
    /// or `None` without a registry.
    pub(crate) fn validate_against_head(
        &self,
        collection: &str,
        document: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, String> {
        let Some(head) = self.head(collection)? else {
            return Ok(None);
        };
        let mut body = document.clone();
        body.remove(Self::VERSION_FIELD);
        head.validate(collection, &body)?;
        body.insert(
            Self::VERSION_FIELD.into(),
            Value::String(head.label().to_owned()),
        );
        Ok(Some(body))
    }

    /// Commit `record`, which must follow the head; `verify` runs first with
    /// the collection held against other writers.
    pub(crate) fn commit(
        &self,
        collection: &str,
        record: &RegistryVersion,
        verify: &mut dyn FnMut() -> Result<(), NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        let mut bytes = serde_json::to_vec_pretty(record)
            .map_err(|error| NativeStorageError::codec_rejected(error.to_string()))?;
        bytes.push(b'\n');
        self.store
            .commit(collection, record.ordinal, &bytes, verify)?;
        if let Some(heads) = &self.heads {
            Self::lock(heads).remove(collection);
        }
        Ok(())
    }

    pub(crate) fn import_record(&self) -> Result<Option<Vec<u8>>, NativeStorageError> {
        self.store.import_record()
    }

    pub(crate) fn write_import_record(&self, record: &[u8]) -> Result<(), NativeStorageError> {
        self.store.write_import_record(record)
    }

    pub(crate) const VERSION_FIELD: &'static str = "_v";

    /// Migrate `document` from its `_v` (the first version when it has none)
    /// to the last of `versions`, which run from ordinal 1 in order. The
    /// errors match `fylo.schema` materialization's.
    pub(crate) fn materialize_through(
        collection: &str,
        versions: &[Arc<LoadedVersion>],
        document: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let Some((target, _)) = versions.split_last() else {
            return Ok(document.clone());
        };
        let from = document
            .get(Self::VERSION_FIELD)
            .and_then(Value::as_str)
            .unwrap_or(versions[0].label());
        let start = versions
            .iter()
            .position(|version| version.label() == from)
            .ok_or_else(|| {
                format!(
                    "Doc in '{collection}' is at unknown version '{from}' (not in the schema registry)"
                )
            })?;
        if start + 1 == versions.len() {
            return Ok(document.clone());
        }
        let mut next = document.clone();
        next.remove(Self::VERSION_FIELD);
        for pair in versions[start..].windows(2) {
            pair[1].migrate(&mut next).map_err(|error| {
                format!(
                    "migration {}->{} of '{collection}' failed: {error}",
                    pair[0].label(),
                    pair[1].label()
                )
            })?;
        }
        next.insert(
            Self::VERSION_FIELD.into(),
            Value::String(target.label().to_owned()),
        );
        Ok(next)
    }

    fn loaded() -> &'static Mutex<LoadedVersions> {
        static LOADED: OnceLock<Mutex<LoadedVersions>> = OnceLock::new();
        LOADED.get_or_init(Mutex::default)
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
