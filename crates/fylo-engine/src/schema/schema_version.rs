use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct SchemaVersion {
    #[serde(rename = "v")]
    pub(super) version: String,
    #[serde(rename = "addedAt", default, skip_serializing_if = "Option::is_none")]
    pub(super) added_at: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sha256: Option<String>,
}

impl SchemaVersion {
    pub(super) fn matches(&self, label: &str) -> bool {
        self.version == label
    }
}
