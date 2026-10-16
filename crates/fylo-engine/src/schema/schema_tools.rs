use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::schema_diagnosis::SchemaDiagnosis;
use super::schema_inspection::SchemaInspection;
use super::schema_manifest::SchemaManifest;
use super::schema_paths::SchemaPaths;
use super::schema_upgrade::SchemaUpgrade;
use super::schema_validation::SchemaValidation;

pub(crate) struct SchemaTools {
    schema_dir: PathBuf,
    validator: SchemaValidation,
}

impl SchemaTools {
    pub(crate) fn new(schema_dir: impl Into<PathBuf>) -> Self {
        Self {
            schema_dir: schema_dir.into(),
            validator: SchemaValidation::new(),
        }
    }

    pub(crate) fn schema_dir(&self) -> &Path {
        &self.schema_dir
    }

    pub(crate) fn with_chex_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.validator.set_binary(binary);
        self
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
        let mut body = document.clone();
        body.remove(SchemaUpgrade::VERSION_FIELD);
        let paths = SchemaPaths::new(&self.schema_dir, collection);
        let manifest = SchemaManifest::load(&paths)?;
        self.validate_known(&paths, manifest.as_ref(), &body)
    }

    fn materialize_known(
        paths: &SchemaPaths<'_>,
        manifest: Option<&SchemaManifest>,
        document: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        match manifest {
            Some(manifest) => SchemaUpgrade::new(paths, manifest).materialize(document),
            None => Ok(document.clone()),
        }
    }

    fn validate_known(
        &self,
        paths: &SchemaPaths<'_>,
        manifest: Option<&SchemaManifest>,
        body: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, String> {
        match manifest {
            Some(manifest) => self.validate_version(paths, manifest, body).map(Some),
            None => self.validate_flat(paths, body),
        }
    }

    fn validate_flat(
        &self,
        paths: &SchemaPaths<'_>,
        body: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, String> {
        let exists = paths.flat().is_file();
        self.validate_existing_flat(paths, body, exists)
    }

    fn validate_existing_flat(
        &self,
        paths: &SchemaPaths<'_>,
        body: &Map<String, Value>,
        exists: bool,
    ) -> Result<Option<Map<String, Value>>, String> {
        if !exists {
            return Ok(None);
        }
        self.validator
            .validate(paths.collection, body, Some(paths.root))
            .map(Some)
    }

    fn validate_version(
        &self,
        paths: &SchemaPaths<'_>,
        manifest: &SchemaManifest,
        body: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        let head = paths.version(&manifest.current);
        Self::require_head(&head)?;
        let mut validated = self
            .validator
            .validate(&head.to_string_lossy(), body, None)?;
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
