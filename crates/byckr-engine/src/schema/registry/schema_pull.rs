use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::schema_registry::SchemaRegistry;
use crate::EngineError;
use crate::schema::schema_paths::SchemaPaths;

/// `fylo schema pull`: write a collection's registry back out as the
/// `fylo.schema` folder layout, so it can be edited and pushed again.
pub(crate) struct SchemaPull;

impl SchemaPull {
    pub(crate) fn run(
        registry: &SchemaRegistry,
        collection: &str,
        out: &Path,
    ) -> Result<Value, EngineError> {
        let versions = registry.versions(collection).map_err(EngineError::schema)?;
        let Some(head) = versions.last() else {
            return Err(EngineError::schema(format!(
                "collection {collection} has no schema registry to pull"
            )));
        };
        let paths = SchemaPaths::new(out, collection);
        let history = paths.version(head.label());
        let history = history.parent().expect("a version file has a directory");
        let migrations = paths.migration(head.label(), head.label());
        let migrations = migrations
            .parent()
            .expect("a migration file has a directory");
        for directory in [history, migrations] {
            fs::create_dir_all(directory).map_err(|error| Self::io(directory, &error))?;
        }
        let mut entries = Vec::new();
        for (index, version) in versions.iter().enumerate() {
            let schema = Self::pretty(&version.record.chex)?;
            Self::write(&paths.version(version.label()), &schema)?;
            entries.push(json!({
                "v": version.label(),
                "addedAt": version.record.committed.at,
                "sha256": Self::file_digest(&schema),
            }));
            if let (Some(previous), Some(migration)) = (
                index.checked_sub(1).map(|previous| &versions[previous]),
                version.record.migration.as_ref(),
            ) {
                Self::write(
                    &paths.migration(previous.label(), version.label()),
                    &Self::pretty(migration)?,
                )?;
            }
        }
        let manifest = json!({ "current": head.label(), "versions": entries });
        Self::write(&paths.manifest(), &Self::pretty(&manifest)?)?;
        Ok(json!({
            "collection": collection,
            "out": out,
            "current": head.label(),
            "versions": versions.len(),
        }))
    }

    fn pretty(value: &Value) -> Result<Vec<u8>, EngineError> {
        let mut bytes = serde_json::to_vec_pretty(value)
            .map_err(|error| EngineError::schema(error.to_string()))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn write(path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
        fs::write(path, bytes).map_err(|error| Self::io(path, &error))
    }

    fn io(path: &Path, error: &std::io::Error) -> EngineError {
        EngineError::schema(format!("cannot write {}: {error}", path.display()))
    }

    /// The manifest's `sha256` is of the file's bytes, as `schema doctor`
    /// checks it.
    fn file_digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |mut text, byte| {
                use std::fmt::Write as _;
                let _ = write!(text, "{byte:02x}");
                text
            })
    }
}
