use serde::Deserialize;
use serde::de::IgnoredAny;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StoredMessageHeader {
    pub(super) format: String,
    pub(super) id: String,
    pub(super) topic: String,
    pub(super) published_at: u64,
    pub(super) available_at: u64,
    #[serde(rename = "payload")]
    pub(super) _payload: IgnoredAny,
}
