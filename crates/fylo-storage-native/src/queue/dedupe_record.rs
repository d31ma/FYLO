use serde::{Deserialize, Serialize};

use super::stored_message::StoredMessage;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DedupeRecord {
    pub(super) format: String,
    pub(super) topic: String,
    pub(super) payload_hash: String,
    pub(super) message: StoredMessage,
}
