//! Schema registry administration for the write engine: `fylo schema push`
//! and `pull`, the one-time import of a `fylo.schema` folder, and
//! `fylo.schema.autoPush` (ADR 0017 section 3).

use std::path::Path;
use std::sync::Arc;

use fylo_storage_native::NativeStorageErrorCode;
use serde_json::{Map, Value, json};

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
        SchemaPush::new(&self.registry, schema_dir, collection).run(actor, Some(&mut documents))
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
