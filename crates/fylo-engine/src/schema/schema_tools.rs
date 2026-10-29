use std::path::{Path, PathBuf};

use fylo_format::Document;
use serde_json::{Map, Value};

use super::schema_diagnosis::SchemaDiagnosis;
use super::schema_inspection::SchemaInspection;
use super::schema_manifest::SchemaManifest;
use super::schema_paths::SchemaPaths;
use super::schema_upgrade::SchemaUpgrade;
use super::schema_validation::SchemaValidation;

pub(crate) struct SchemaTools {
    schema_dir: PathBuf,
}

impl SchemaTools {
    pub(crate) fn new(schema_dir: impl Into<PathBuf>) -> Self {
        Self {
            schema_dir: schema_dir.into(),
        }
    }

    pub(crate) fn schema_dir(&self) -> &Path {
        &self.schema_dir
    }

    pub(crate) fn inspect(&self, collection: &str) -> Result<Value, String> {
        let paths = SchemaPaths::new(&self.schema_dir, collection);
        Ok(SchemaInspection::load(&paths)?.into_value())
    }

    pub(crate) fn doctor(&self, collection: &str) -> Value {
        let paths = SchemaPaths::new(&self.schema_dir, collection);
        SchemaDiagnosis::load(&paths).into_value()
    }

    pub(crate) fn current_version(&self, collection: &str) -> Result<Option<String>, String> {
        let paths = SchemaPaths::new(&self.schema_dir, collection);
        Ok(SchemaManifest::load(&paths)?.map(SchemaManifest::current))
    }

    /// Whether the folder may hold a schema for `collection`: a manifest or a
    /// flat schema file, or a path this build cannot inspect (any path in a
    /// browser), which a build that cannot validate must assume holds one.
    #[cfg(any(test, target_arch = "wasm32"))]
    pub(crate) fn may_have_schema(&self, collection: &str) -> bool {
        let paths = SchemaPaths::new(&self.schema_dir, collection);
        [paths.manifest(), paths.flat()].iter().any(|path| {
            std::fs::symlink_metadata(path).map_or_else(
                |error| error.kind() != std::io::ErrorKind::NotFound,
                |_| true,
            )
        })
    }

    pub(crate) fn materialize(
        &self,
        collection: &str,
        document: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let paths = SchemaPaths::new(&self.schema_dir, collection);
        let manifest = SchemaManifest::load(&paths)?;
        Self::materialize_known(&paths, manifest.as_ref(), document)
    }

    pub(crate) fn validate_against_head(
        &self,
        collection: &str,
        document: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, String> {
        let mut body = Document::copy_fields(document);
        body.remove(SchemaUpgrade::VERSION_FIELD);
        let paths = SchemaPaths::new(&self.schema_dir, collection);
        let manifest = SchemaManifest::load(&paths)?;
        Self::validate_known(&paths, manifest.as_ref(), &body)
    }

    /// `document` with the `_v` a write stores: the head label of a versioned
    /// collection, none for a flat schema, and whatever the document carries
    /// when the collection has no schema. This is the stamp
    /// [`Self::validate_against_head`] returns, applied without validating, so
    /// a write stores the same bytes whatever its validation mode.
    pub(crate) fn stamp_head(
        &self,
        collection: &str,
        mut document: Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let paths = SchemaPaths::new(&self.schema_dir, collection);
        match SchemaManifest::load(&paths)? {
            Some(manifest) => {
                document.insert(
                    SchemaUpgrade::VERSION_FIELD.into(),
                    Value::String(manifest.current),
                );
            }
            None if paths.flat().is_file() => {
                document.remove(SchemaUpgrade::VERSION_FIELD);
            }
            None => {}
        }
        Ok(document)
    }

    fn materialize_known(
        paths: &SchemaPaths<'_>,
        manifest: Option<&SchemaManifest>,
        document: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        match manifest {
            Some(manifest) => SchemaUpgrade::new(paths, manifest).materialize(document),
            None => Ok(Document::copy_fields(document)),
        }
    }

    fn validate_known(
        paths: &SchemaPaths<'_>,
        manifest: Option<&SchemaManifest>,
        body: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, String> {
        match manifest {
            Some(manifest) => Self::validate_version(paths, manifest, body).map(Some),
            None => Self::validate_flat(paths, body),
        }
    }

    fn validate_flat(
        paths: &SchemaPaths<'_>,
        body: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, String> {
        let exists = paths.flat().is_file();
        Self::validate_existing_flat(paths, body, exists)
    }

    fn validate_existing_flat(
        paths: &SchemaPaths<'_>,
        body: &Map<String, Value>,
        exists: bool,
    ) -> Result<Option<Map<String, Value>>, String> {
        if !exists {
            return Ok(None);
        }
        SchemaValidation::validate(paths.collection, body, Some(paths.root)).map(Some)
    }

    fn validate_version(
        paths: &SchemaPaths<'_>,
        manifest: &SchemaManifest,
        body: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let head = paths.version(&manifest.current);
        Self::require_head(&head)?;
        let mut validated = SchemaValidation::validate(&head.to_string_lossy(), body, None)?;
        validated.insert(
            SchemaUpgrade::VERSION_FIELD.into(),
            Value::String(manifest.current.clone()),
        );
        Ok(validated)
    }

    fn require_head(head: &Path) -> Result<(), String> {
        if !head.is_file() {
            return Err(format!(
                "head schema is missing: {}",
                head.to_string_lossy()
            ));
        }
        Ok(())
    }
}
