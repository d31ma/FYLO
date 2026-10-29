use serde_json::{Map, Value};

use super::migration_step::MigrationStep;
use super::schema_paths::SchemaPaths;
use super::utils::schema_files::SchemaFiles;

/// The JSON migration between two adjacent schema versions (ADR 0018):
/// `migrations/<from>-to-<to>.json`, holding `{"steps": [...]}`.
pub(super) struct SchemaMigration {
    steps: Vec<MigrationStep>,
}

impl SchemaMigration {
    pub(super) fn load(paths: &SchemaPaths<'_>, from: &str, to: &str) -> Result<Self, String> {
        let path = paths.migration(from, to);
        let bytes = SchemaFiles::read(&path).map_err(|_| Self::missing(paths, from, to))?;
        let source: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("migration {} is not valid JSON: {error}", path.display()))?;
        Self::parse(&source)
            .map_err(|error| format!("migration {} is invalid: {error}", path.display()))
    }

    pub(super) fn parse(source: &Value) -> Result<Self, String> {
        let fields = source
            .as_object()
            .ok_or("a migration must be an object with a \"steps\" array")?;
        if let Some(key) = fields.keys().find(|key| *key != "steps") {
            return Err(format!("a migration does not take {key:?}"));
        }
        let steps = fields
            .get("steps")
            .and_then(Value::as_array)
            .ok_or("a migration needs a \"steps\" array")?
            .iter()
            .enumerate()
            .map(|(index, step)| {
                MigrationStep::parse(step).map_err(|error| format!("step {}: {error}", index + 1))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { steps })
    }

    pub(super) fn apply(&self, document: &mut Map<String, Value>) -> Result<(), String> {
        self.steps.iter().enumerate().try_for_each(|(index, step)| {
            step.apply(document)
                .map_err(|error| format!("step {}: {error}", index + 1))
        })
    }

    fn missing(paths: &SchemaPaths<'_>, from: &str, to: &str) -> String {
        let legacy = paths.legacy_upgrader(from, to);
        if legacy.is_file() {
            return format!(
                "Collection '{}' has a JavaScript upgrader {from}->{to} at {}, which FYLO no longer runs; \
                 rewrite it as {} (see ADR 0018)",
                paths.collection,
                legacy.display(),
                paths.migration(from, to).display()
            );
        }
        format!(
            "Missing migration {from}->{to} for collection '{}' at {}",
            paths.collection,
            paths.migration(from, to).display()
        )
    }
}
