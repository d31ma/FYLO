use std::fs;
use std::path::Path;

use serde_json::{Map, json};

use super::schema_push::SchemaPush;
use super::schema_registry::SchemaRegistry;
use crate::EngineError;

/// The one-time import of a `fylo.schema` folder into a root's registry
/// (ADR 0017 section 3), so that upgrading to a binary that reads schemas
/// from the registry does not silently drop validation.
///
/// The first time a write engine with a schema folder opens a root that has
/// no import record, every collection the folder versions and the root has,
/// without a registry yet, is pushed, and the record is written whatever
/// happened. Collections created afterwards keep the folder until `fylo
/// schema push`, and the import never runs again.
pub(crate) struct SchemaImport;

impl SchemaImport {
    pub(crate) const FORMAT: &'static str = "fylo.schema-import.v1";
    const ACTOR: &'static str = "fylo.schema import";

    pub(crate) fn run_once(
        registry: &SchemaRegistry,
        schema_dir: &Path,
        exists: &dyn Fn(&str) -> bool,
    ) -> Result<(), EngineError> {
        if registry
            .import_record()
            .map_err(EngineError::storage)?
            .is_some()
        {
            return Ok(());
        }
        let mut outcomes = Map::new();
        for collection in Self::folder_collections(schema_dir)? {
            let needed = exists(&collection)
                && registry
                    .head_ordinal(&collection)
                    .map_err(EngineError::schema)?
                    .is_none();
            if !needed {
                continue;
            }
            // The documents were written under these very schemas, so the
            // import adopts them without a dry run. A version the registry
            // cannot hold leaves the collection on the folder, as before.
            let outcome =
                match SchemaPush::new(registry, schema_dir, &collection).run(Self::ACTOR, None) {
                    Ok(report) => json!({ "pushed": report.pushed }),
                    Err(error) => json!({ "error": error.to_string() }),
                };
            outcomes.insert(collection, outcome);
        }
        let record = json!({
            "format": Self::FORMAT,
            "at": fylo_storage_native::utc_timestamp().map_err(EngineError::storage)?,
            "schemaDir": schema_dir,
            "collections": outcomes,
        });
        let mut bytes = serde_json::to_vec_pretty(&record)
            .map_err(|error| EngineError::schema(error.to_string()))?;
        bytes.push(b'\n');
        registry
            .write_import_record(&bytes)
            .map_err(EngineError::storage)
    }

    /// Collections the folder versions: directories holding a `manifest.json`,
    /// in name order.
    pub(crate) fn folder_collections(schema_dir: &Path) -> Result<Vec<String>, EngineError> {
        let entries = fs::read_dir(schema_dir).map_err(|error| {
            EngineError::schema(format!("cannot list {}: {error}", schema_dir.display()))
        })?;
        let mut collections = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| EngineError::schema(error.to_string()))?;
            let versioned = entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry.path().join("manifest.json").is_file();
            if let (true, Some(name)) = (versioned, entry.file_name().to_str()) {
                collections.push(name.to_owned());
            }
        }
        collections.sort();
        Ok(collections)
    }
}
