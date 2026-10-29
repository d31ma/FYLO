//! Convergence counts (ADR 0017 section 6): how many live documents of a
//! collection are stored at each schema version.
//!
//! The count of a directory root lives beside the collection's schema
//! registry, `<root>/.fylo-schemas/<collection>/counts.json`, stamped with the
//! collection generation it describes. A branch worktree shares the registry
//! but not the documents, so it keeps its own file there,
//! `counts.<worktree>.json`. Storage counts the stored `_v` label of each live
//! document and nothing else: which label is the head, and so whether the
//! collection has converged, is the engine's business.
//!
//! The counts are derived. A count whose stamp is not the collection's
//! current stable generation is stale and is never trusted: the writer counts
//! again by scan at the next commit, and a reader counts by scan in memory. A
//! commit to a collection whose registry has more than one version keeps the
//! count current from the pre-image and post-image `_v` of every live
//! document it changes; anything else that moves the generation (recovery, a
//! rolled-back write, an operation that does not keep the count) leaves the
//! stamp behind, which is how it is detected. The file is replaced
//! atomically but not synchronously: a count lost or torn by a crash is
//! detected as missing or stale and rebuilt, so no commit waits on it.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use fylo_vfs::{self as fs, OpenOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{NativeWriteRoot, json_error, sibling_scratch};
use crate::{
    CollectionKind, GenerationStatus, MAX_DESCRIPTOR_BYTES, NativeCollection, NativeStorageError,
    NativeStorageErrorCode, path_exists_no_follow,
};

/// Live documents of one collection by the `_v` label they are stored at.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaVersionCounts {
    /// Documents by their stored string `_v`.
    pub versions: BTreeMap<String, u64>,
    /// Documents with no string `_v`, which the engine reads as the oldest
    /// version.
    pub unversioned: u64,
}

impl SchemaVersionCounts {
    /// Every live document counted.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.versions.values().sum::<u64>() + self.unversioned
    }

    /// Documents stored at `label`.
    #[must_use]
    pub fn at(&self, label: &str) -> u64 {
        self.versions.get(label).copied().unwrap_or(0)
    }

    fn add(&mut self, label: Option<&str>) {
        match label {
            Some(label) => *self.versions.entry(label.to_owned()).or_default() += 1,
            None => self.unversioned += 1,
        }
    }

    /// Apply a commit's change, or `None` when it would go negative, which
    /// means the count did not describe the documents the commit saw.
    pub(crate) fn applied(mut self, delta: &SchemaCountDelta) -> Option<Self> {
        for (label, change) in &delta.0 {
            let slot = match label {
                Some(label) => self.versions.entry(label.clone()).or_default(),
                None => &mut self.unversioned,
            };
            *slot = slot.checked_add_signed(*change)?;
        }
        self.versions.retain(|_, count| *count > 0);
        Some(self)
    }

    /// The `_v` label stored in a document body, or `None` without a string
    /// `_v`.
    ///
    /// # Errors
    ///
    /// Returns an error for a body that is not a JSON object.
    pub fn label_of(bytes: &[u8]) -> Result<Option<String>, NativeStorageError> {
        #[derive(Deserialize)]
        struct Probe {
            #[serde(rename = "_v", default)]
            version: Option<Value>,
        }
        let probe: Probe = serde_json::from_slice(bytes).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptDocument,
                format!("stored document is not a JSON object: {error}"),
            )
        })?;
        Ok(match probe.version {
            Some(Value::String(label)) => Some(label),
            _ => None,
        })
    }
}

/// How one commit changes the count, by label; `None` is unversioned.
#[derive(Clone, Debug, Default)]
pub(crate) struct SchemaCountDelta(BTreeMap<Option<String>, i64>);

impl SchemaCountDelta {
    pub(crate) fn remove(&mut self, bytes: &[u8]) -> Result<(), NativeStorageError> {
        *self
            .0
            .entry(SchemaVersionCounts::label_of(bytes)?)
            .or_default() -= 1;
        Ok(())
    }

    pub(crate) fn add(&mut self, bytes: &[u8]) -> Result<(), NativeStorageError> {
        *self
            .0
            .entry(SchemaVersionCounts::label_of(bytes)?)
            .or_default() += 1;
        Ok(())
    }

    pub(crate) fn merge(&mut self, other: &Self) {
        for (label, change) in &other.0 {
            *self.0.entry(label.clone()).or_default() += change;
        }
    }

    /// Whether `relative`, a path inside a collection, is a live document
    /// body: `docs/<shard>/<id>.json`, never a sidecar.
    pub(crate) fn is_live_document(relative: &Path) -> bool {
        let mut components = relative.components();
        components.next() == Some(Component::Normal("docs".as_ref()))
            && relative
                .extension()
                .is_some_and(|extension| extension == "json")
            && relative
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.ends_with(crate::SIDECAR_SUFFIX))
    }
}

/// The file that records the count.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CountsRecord {
    format: String,
    generation: u64,
    #[serde(flatten)]
    counts: SchemaVersionCounts,
}

impl CountsRecord {
    const FORMAT: &'static str = "fylo.schema-counts.v1";
}

impl NativeCollection {
    /// Whether the writer keeps this collection's count: a document
    /// collection whose schema registry has a migration to run.
    #[must_use]
    pub(crate) fn tracks_schema_versions(&self) -> bool {
        self.kind == CollectionKind::Document && self.schema_head.is_some_and(|head| head >= 2)
    }

    /// The count recorded at this collection's current stable generation, or
    /// `None` when there is none, it is stale, or it cannot be read.
    ///
    /// # Errors
    ///
    /// Returns an error when the generation cannot be read.
    pub fn recorded_schema_versions(
        &self,
    ) -> Result<Option<SchemaVersionCounts>, NativeStorageError> {
        let state = self.generation()?;
        if state.state != GenerationStatus::Stable {
            return Ok(None);
        }
        Ok(self.read_schema_counts(state.generation))
    }

    /// Count every live document by its stored `_v`.
    ///
    /// # Errors
    ///
    /// Returns an error for a file collection, a document that is not a JSON
    /// object, or I/O.
    pub fn scan_schema_versions(&self) -> Result<SchemaVersionCounts, NativeStorageError> {
        let mut counts = SchemaVersionCounts::default();
        for identifier in self.document_ids()? {
            let stored = match self.read_document(&identifier) {
                Ok(stored) => stored,
                // Deleted since it was listed; a caller that needs a stable
                // answer checks the generation around the scan.
                Err(error) if error.code() == NativeStorageErrorCode::NotFound => continue,
                Err(error) => return Err(error),
            };
            counts.add(SchemaVersionCounts::label_of(&stored.bytes)?.as_deref());
        }
        Ok(counts)
    }

    /// The count recorded at `generation`, if the file says so. A caller that
    /// read the stable generation itself uses this in place of
    /// [`Self::recorded_schema_versions`].
    #[must_use]
    pub fn read_schema_counts(&self, generation: u64) -> Option<SchemaVersionCounts> {
        let path = self.schema_counts_path();
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_DESCRIPTOR_BYTES
        {
            return None;
        }
        let record: CountsRecord = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
        (record.format == CountsRecord::FORMAT && record.generation == generation)
            .then_some(record.counts)
    }

    /// `<root>/.fylo-schemas/<collection>/counts.json` for the root that owns
    /// the catalog, and a file named after the worktree for a branch.
    pub(crate) fn schema_counts_path(&self) -> PathBuf {
        let directory = self.root.schema_registry_directory().join(&self.name);
        let worktree = self
            .root
            .canonical
            .strip_prefix(self.root.catalog_root())
            .unwrap_or(&self.root.canonical);
        if worktree.as_os_str().is_empty() {
            return directory.join("counts.json");
        }
        let digest = Sha256::digest(worktree.to_string_lossy().as_bytes());
        let name = digest[..8]
            .iter()
            .fold(String::with_capacity(16), |mut name, byte| {
                use std::fmt::Write as _;
                let _ = write!(name, "{byte:02x}");
                name
            });
        directory.join(format!("counts.{name}.json"))
    }
}

impl NativeWriteRoot {
    /// Drop the counts of each existing collection in `names`.
    pub(crate) fn invalidate_schema_counts_of<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), NativeStorageError> {
        for name in names {
            if let Ok(handle) = self.root.collection(name) {
                Self::invalidate_schema_counts(&handle)?;
            }
        }
        Ok(())
    }

    /// Count `collection` by scan under its lock, after recovery, and record
    /// the count at its current generation.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing or file collection, a document that is
    /// not a JSON object, or I/O.
    pub fn rebuild_schema_versions(
        &self,
        collection_name: &str,
    ) -> Result<SchemaVersionCounts, NativeStorageError> {
        self.root.assert_profile_owned()?;
        let collection = self.root.collection(collection_name)?;
        let _lock = self.lock_collection(&collection.path)?;
        self.recover_locked(&collection)?;
        let collection = self.root.collection(collection_name)?;
        let generation = collection.stored_generation()?.generation;
        let counts = collection.scan_schema_versions()?;
        if path_exists_no_follow(&self.root.schema_registry_directory().join(collection_name))? {
            Self::record_schema_counts(&collection, generation, &counts)?;
        }
        Ok(counts)
    }

    /// The count a tracked commit starts from: the recorded one when it is
    /// current at `generation`, else a fresh scan, recorded at `generation`.
    /// The caller holds the collection lock with the generation stable.
    /// `None` for a collection the writer does not track, or when the count
    /// cannot be had, in which case the commit leaves it stale.
    pub(crate) fn schema_counts_before_commit(
        collection: &NativeCollection,
        generation: u64,
    ) -> Option<SchemaVersionCounts> {
        if !collection.tracks_schema_versions() {
            return None;
        }
        if let Some(counts) = collection.read_schema_counts(generation) {
            return Some(counts);
        }
        let counts = collection.scan_schema_versions().ok()?;
        Self::record_schema_counts(collection, generation, &counts).ok()?;
        Some(counts)
    }

    /// Record the count after a commit published `generation`. Failure is
    /// not the commit's: the stamp stays behind and the count reads as stale.
    pub(crate) fn schema_counts_after_commit(
        collection: &NativeCollection,
        before: SchemaVersionCounts,
        delta: &SchemaCountDelta,
        generation: u64,
    ) {
        if let Some(after) = before.applied(delta) {
            let _ = Self::record_schema_counts(collection, generation, &after);
        }
    }

    /// Forget a collection's count, before an operation that replaces its
    /// documents wholesale.
    pub(crate) fn invalidate_schema_counts(
        collection: &NativeCollection,
    ) -> Result<(), NativeStorageError> {
        let path = collection.schema_counts_path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn record_schema_counts(
        collection: &NativeCollection,
        generation: u64,
        counts: &SchemaVersionCounts,
    ) -> Result<(), NativeStorageError> {
        let path = collection.schema_counts_path();
        let directory = path.parent().ok_or_else(|| {
            NativeStorageError::new(NativeStorageErrorCode::UnsafePath, "counts have no parent")
        })?;
        let metadata = fs::symlink_metadata(directory).map_err(NativeStorageError::io)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "schema registry path is not a directory",
            ));
        }
        let record = CountsRecord {
            format: CountsRecord::FORMAT.into(),
            generation,
            counts: counts.clone(),
        };
        let mut bytes = serde_json::to_vec(&record).map_err(|error| json_error(&error))?;
        bytes.push(b'\n');
        let scratch = sibling_scratch(&path);
        let outcome = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&scratch)
                .map_err(NativeStorageError::io)?;
            file.write_all(&bytes).map_err(NativeStorageError::io)?;
            drop(file);
            fs::rename(&scratch, &path).map_err(NativeStorageError::io)
        })();
        if outcome.is_err() {
            let _ = fs::remove_file(&scratch);
        }
        outcome
    }
}
