//! Schema registry administration for the write engine: `fylo schema push`
//! and `pull`, the one-time import of a `fylo.schema` folder, and
//! `fylo.schema.autoPush` (ADR 0017 section 3).

use std::path::Path;
use std::sync::Arc;

use fylo_storage_native::{CollectionKind, NativeStorageErrorCode, SchemaVersionCounts};
use serde_json::{Map, Value, json};

use crate::lazy_migration::Convergence;

use crate::schema::{
    LoadedVersion, SchemaImport, SchemaPull, SchemaPush, SchemaPushReport, SchemaTools,
};
use crate::{EngineError, EngineErrorCode, WriteEngine};

impl WriteEngine {
    /// The actor a push records when its caller names none.
    const PUSH_ACTOR: &'static str = "fylo schema push";
    const AUTO_PUSH_ACTOR: &'static str = "fylo.schema autoPush";

    /// `collection`'s head version in the root's schema registry, or `None`
    /// when the collection has no registry and its schemas come from the
    /// `fylo.schema` folder.
    pub(crate) fn registry_head(
        &self,
        collection: &str,
    ) -> Result<Option<Arc<LoadedVersion>>, EngineError> {
        self.registry.head(collection).map_err(EngineError::schema)
    }

    /// The configured `fylo.schema` folder, if any.
    #[must_use]
    pub fn schema_folder(&self) -> Option<&Path> {
        self.schema.as_deref().map(SchemaTools::schema_dir)
    }

    /// `fylo schema push`: commit each version of the `fylo.schema` folder
    /// (`schema_dir`, else the configured one) that `collection`'s registry
    /// lacks, in manifest order, after migrating the collection's documents
    /// to the new head and validating them there.
    ///
    /// # Errors
    ///
    /// Returns `ESCHEMACONFLICT` when a committed label's content changed,
    /// `EARRAYOFOBJECTS` for an array whose items are objects, `ESCHEMA` for
    /// an invalid folder or a dry run that fails, and a storage error when the
    /// collection is missing or the commit fails.
    pub fn schema_push(
        &self,
        collection: &str,
        schema_dir: Option<&Path>,
        actor: Option<&str>,
    ) -> Result<Value, EngineError> {
        let schema_dir = match schema_dir {
            Some(schema_dir) => schema_dir,
            None => self.schema_tools()?.schema_dir(),
        };
        let report = self.push(collection, schema_dir, actor.unwrap_or(Self::PUSH_ACTOR))?;
        serde_json::to_value(report).map_err(|error| EngineError::schema(error.to_string()))
    }

    fn push(
        &self,
        collection: &str,
        schema_dir: &Path,
        actor: &str,
    ) -> Result<SchemaPushReport, EngineError> {
        // Fail before reading the folder when the collection does not exist,
        // so a typo is reported as the storage error it is.
        let handle = self
            .writer
            .native_root()
            .collection(collection)
            .map_err(EngineError::storage)?;
        let mut documents = || {
            let identifiers = handle.document_ids().map_err(EngineError::storage)?;
            let total = identifiers.len();
            let mut sample = Vec::new();
            for identifier in SchemaPush::sample(identifiers) {
                if let Some(fields) = self.stored_fields(&handle, collection, &identifier)? {
                    sample.push((identifier, fields));
                }
            }
            Ok((total, sample))
        };
        let report = SchemaPush::new(&self.registry, schema_dir, collection)
            .run(actor, Some(&mut documents))?;
        // A head with a migration starts the convergence count (ADR 0017
        // section 6). The writer would count at its next commit anyway, and
        // a reader by scan, so a failure here is not the push's.
        if self
            .registry
            .head_ordinal(collection)
            .is_ok_and(|head| head.is_some_and(|head| head >= 2))
        {
            let _ = self.writer.rebuild_schema_versions(collection);
        }
        Ok(report)
    }

    /// Live documents of `collection` by the version they are stored at, or
    /// `None` when it is not an existing document collection. A count the
    /// writer keeps is recorded again when it is stale.
    fn schema_version_counts(
        &self,
        collection: &str,
    ) -> Result<Option<SchemaVersionCounts>, EngineError> {
        let handle = match self.writer.native_root().collection(collection) {
            Ok(handle) => handle,
            Err(error)
                if matches!(
                    error.code(),
                    NativeStorageErrorCode::NotFound | NativeStorageErrorCode::InvalidCollection
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(EngineError::storage(error)),
        };
        if handle.kind() != CollectionKind::Document {
            return Ok(None);
        }
        if let Some(counts) = handle
            .recorded_schema_versions()
            .map_err(EngineError::storage)?
        {
            return Ok(Some(counts));
        }
        let counts = if handle.schema_head().is_some() {
            self.writer.rebuild_schema_versions(collection)
        } else {
            handle.scan_schema_versions()
        };
        counts.map(Some).map_err(EngineError::storage)
    }

    /// `report`, a `schemaInspect` result, with the collection's validation
    /// mode and where its documents stand against the head (ADR 0017
    /// section 6): the documents at each version, those without `_v`
    /// (counted at the oldest), those at a version the schema does not list,
    /// and whether every document is at the head.
    pub(crate) fn with_convergence(
        &self,
        collection: &str,
        mut report: Value,
    ) -> Result<Value, EngineError> {
        let Some(counts) = self.schema_version_counts(collection)? else {
            return Ok(report);
        };
        let head = report["current"].as_str().map(str::to_owned);
        let labels: Vec<String> = report["versions"]
            .as_array()
            .map(|versions| {
                versions
                    .iter()
                    .filter_map(|entry| entry["version"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let (per_version, unknown, converged) =
            Convergence::report(&labels, head.as_deref(), &counts);
        if let Some(versions) = report["versions"].as_array_mut() {
            for (entry, documents) in versions.iter_mut().zip(per_version) {
                entry["documents"] = json!(documents);
            }
        }
        let mode = self.validation_mode(collection)?;
        let Some(fields) = report.as_object_mut() else {
            return Ok(report);
        };
        fields.insert("validation".into(), json!(mode.as_str()));
        fields.insert("head".into(), json!(head));
        fields.insert("documents".into(), json!(counts.total()));
        fields.insert("unversioned".into(), json!(counts.unversioned));
        if !unknown.is_empty() {
            fields.insert("unknownVersions".into(), json!(unknown));
        }
        fields.insert("converged".into(), json!(converged));
        Ok(report)
    }

    /// One stored document, decrypted at the version it was written at, or
    /// `None` when it was deleted since it was listed.
    fn stored_fields(
        &self,
        handle: &fylo_storage_native::NativeCollection,
        collection: &str,
        identifier: &str,
    ) -> Result<Option<Map<String, Value>>, EngineError> {
        let stored = match handle.read_document(identifier) {
            Ok(stored) => stored,
            Err(error) if error.code() == NativeStorageErrorCode::NotFound => return Ok(None),
            Err(error) => return Err(EngineError::storage(error)),
        };
        let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(&stored.bytes) else {
            return Err(EngineError::new(
                EngineErrorCode::CorruptData,
                format!("stored document {identifier} is not a JSON object"),
            ));
        };
        self.decode_fields_as_written(collection, fields).map(Some)
    }

    /// `fylo schema pull`: write `collection`'s registry to `out` in the
    /// `fylo.schema` folder layout.
    ///
    /// # Errors
    ///
    /// Returns an error when the collection has no registry, a version file
    /// is corrupt, or `out` cannot be written.
    pub fn schema_pull(&self, collection: &str, out: &Path) -> Result<Value, EngineError> {
        SchemaPull::run(&self.registry, collection, out)
    }

    /// What an opening write engine does with its `fylo.schema` folder: the
    /// one-time import into the root's registry, then, under
    /// `fylo.schema.autoPush`, a push of every folder collection the root
    /// has. Without a folder this does nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when the import record cannot be read or written, or
    /// an automatic push is refused.
    pub fn synchronize_schemas(&self) -> Result<(), EngineError> {
        let Some(schema_dir) = self.schema_folder() else {
            return Ok(());
        };
        let root = self.writer.native_root();
        let exists = |collection: &str| root.collection(collection).is_ok();
        SchemaImport::run_once(&self.registry, schema_dir, &exists)?;
        if !self.config().schema_auto_push {
            return Ok(());
        }
        for collection in SchemaImport::folder_collections(schema_dir)? {
            if exists(&collection) {
                self.push(&collection, schema_dir, Self::AUTO_PUSH_ACTOR)?;
            }
        }
        Ok(())
    }

    /// `schemaInspect` for a collection whose schemas are in the registry.
    pub(crate) fn registry_inspection(&self, collection: &str) -> Result<Value, EngineError> {
        let versions = self
            .registry
            .versions(collection)
            .map_err(EngineError::schema)?;
        let current = versions.last().map(|version| version.label().to_owned());
        let entries: Vec<Value> = versions
            .iter()
            .map(|version| {
                json!({
                    "version": version.label(),
                    "ordinal": version.ordinal(),
                    "current": Some(version.label()) == current.as_deref(),
                    "sha256": version.record.sha256,
                    "committed": {
                        "at": version.record.committed.at,
                        "actor": version.record.committed.actor,
                    },
                })
            })
            .collect();
        Ok(json!({
            "collection": collection,
            "schemaDir": self.schema_folder(),
            "source": "registry",
            "versioned": true,
            "current": current,
            "versions": entries,
        }))
    }

    /// `schemaDoctor` for a collection whose schemas are in the registry:
    /// every committed version must load, and a configured folder whose
    /// `current` differs from the head is reported as a warning, since the
    /// registry, not the folder, is what writes use.
    pub(crate) fn registry_diagnosis(&self, collection: &str) -> Result<Value, EngineError> {
        let head = self
            .registry
            .head_ordinal(collection)
            .map_err(EngineError::schema)?
            .unwrap_or(0);
        let mut issues = Vec::new();
        let mut label = None;
        for ordinal in 1..=head {
            match self.registry.version(collection, ordinal) {
                Ok(version) => label = Some(version.label().to_owned()),
                Err(error) => issues.push(error),
            }
        }
        let mut warnings = Vec::new();
        if let (Some(schema), Some(label)) = (self.schema.as_deref(), label.as_deref()) {
            match schema.current_version(collection) {
                Ok(Some(folder)) if folder != label => warnings.push(format!(
                    "the fylo.schema folder's current version of {collection} is {folder}, \
                     but the schema registry's head is {label}; run fylo schema push or pull"
                )),
                Ok(_) => {}
                Err(error) => warnings.push(error),
            }
        }
        let inspect = if issues.is_empty() {
            Some(self.registry_inspection(collection)?)
        } else {
            None
        };
        Ok(json!({
            "collection": collection,
            "schemaDir": self.schema_folder(),
            "source": "registry",
            "ok": issues.is_empty(),
            "issues": issues,
            "warnings": warnings,
            "inspect": inspect,
        }))
    }
}
