use serde::Serialize;

/// Result of a durable queue publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuePublishResult {
    /// Stable globally ordered message identifier.
    pub id: String,
    /// Whether an existing idempotent publication was returned.
    pub deduplicated: bool,
}
