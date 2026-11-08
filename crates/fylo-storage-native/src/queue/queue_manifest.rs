use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct QueueManifest {
    pub(super) format: String,
    pub(super) next_sequence: u64,
}
