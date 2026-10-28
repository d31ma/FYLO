use std::path::Path;

use serde_json::{Map, Value};

/// Validates a document against a CHEX schema in-process.
///
/// Each call resolves and reads its schema afresh, as the external `chex exec`
/// loop did per request, so a schema edited between writes takes effect on the
/// next write without restarting anything.
pub(super) struct SchemaValidation;

impl SchemaValidation {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn validate(
        schema: &str,
        data: &Map<String, Value>,
        directory: Option<&Path>,
    ) -> Result<Map<String, Value>, String> {
        let directory = directory.map(|directory| directory.to_string_lossy().into_owned());
        fylo_chex::SchemaValidator::new(None, directory)
            .validate(schema, data)
            .map_err(|error| error.message)?;
        Ok(data.clone())
    }

    #[cfg(target_arch = "wasm32")]
    pub(super) fn validate(
        _schema: &str,
        _data: &Map<String, Value>,
        _directory: Option<&Path>,
    ) -> Result<Map<String, Value>, String> {
        Err("schema validation is not available in WebAssembly builds".into())
    }
}
