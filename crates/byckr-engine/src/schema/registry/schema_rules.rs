use serde_json::Value;

use crate::{EngineError, EngineErrorCode};

/// What a schema must satisfy to be committed (ADR 0017 section 4).
pub(super) struct SchemaRules;

impl SchemaRules {
    /// The one directive a schema may carry at its top level today.
    const ENCRYPTED: &'static str = "$encrypted";

    pub(super) fn check(collection: &str, label: &str, chex: &Value) -> Result<(), EngineError> {
        let Value::Object(fields) = chex else {
            return Err(EngineError::schema(format!(
                "schema {label} of {collection} must be a JSON object"
            )));
        };
        if let Some(key) = fields
            .keys()
            .find(|key| key.starts_with('$') && *key != Self::ENCRYPTED)
        {
            return Err(EngineError::schema(format!(
                "schema {label} of {collection} uses {key}: a top-level `$` key is a directive, \
                 and only {} exists (ADR 0017 section 4)",
                Self::ENCRYPTED
            )));
        }
        for (key, value) in fields.iter().filter(|(key, _)| !key.starts_with('$')) {
            Self::check_value(collection, label, key, value)?;
        }
        Ok(())
    }

    /// Refuse an array whose item schema is an object anywhere in the tree:
    /// the document format stores no such array, so every write would fail.
    fn check_value(
        collection: &str,
        label: &str,
        path: &str,
        value: &Value,
    ) -> Result<(), EngineError> {
        match value {
            Value::Array(items) => {
                if items.iter().any(Value::is_object) {
                    return Err(EngineError::new(
                        EngineErrorCode::ArrayOfObjects,
                        format!(
                            "schema {label} of {collection} declares {path} as an array of objects, \
                             which FYLO documents cannot hold; make each object its own collection"
                        ),
                    ));
                }
                items
                    .iter()
                    .try_for_each(|item| Self::check_value(collection, label, path, item))
            }
            Value::Object(fields) => fields.iter().try_for_each(|(key, nested)| {
                Self::check_value(collection, label, &format!("{path}/{key}"), nested)
            }),
            _ => Ok(()),
        }
    }

    /// Refuse a migration that names a JavaScript upgrader (ADR 0018).
    pub(super) fn check_migration(
        collection: &str,
        label: &str,
        migration: &Value,
    ) -> Result<(), EngineError> {
        let names_script = migration
            .get("steps")
            .and_then(Value::as_array)
            .is_some_and(|steps| steps.iter().any(|step| step.get("js").is_some()));
        if names_script {
            return Err(Self::javascript(collection, label, "a \"js\" step"));
        }
        Ok(())
    }

    pub(super) fn javascript(collection: &str, label: &str, what: &str) -> EngineError {
        EngineError::schema(format!(
            "the migration to {label} of {collection} has {what}; FYLO runs no JavaScript \
             upgraders, so rewrite it as JSON steps (see ADR 0018)"
        ))
    }
}
