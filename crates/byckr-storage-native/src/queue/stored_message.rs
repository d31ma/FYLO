use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StoredMessage {
    pub(super) format: String,
    pub(super) id: String,
    pub(super) topic: String,
    pub(super) published_at: u64,
    pub(super) available_at: u64,
    pub(super) payload: Value,
}
