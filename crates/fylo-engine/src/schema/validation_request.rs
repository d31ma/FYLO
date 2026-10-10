use serde::Serialize;
use serde_json::{Map, Value};
use std::path::Path;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ValidationRequest<'a> {
    data: &'a Map<String, Value>,
    #[serde(rename = "op")]
    operation: &'static str,
    schema: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_dir: Option<String>,
}

impl<'a> ValidationRequest<'a> {
    pub(super) fn new(
        schema: &'a str,
        data: &'a Map<String, Value>,
        directory: Option<&Path>,
    ) -> Self {
        Self {
            operation: "validate",
            schema,
            data,
            schema_dir: directory.map(Self::directory_name),
        }
    }

    fn directory_name(directory: &Path) -> String {
        directory.to_string_lossy().into_owned()
    }
}
