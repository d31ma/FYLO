use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durable group-specific dead-letter record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueueDeadLetter {
    pub(super) format: String,
    /// Source message identifier.
    pub id: String,
    /// Consumer group that exhausted delivery.
    pub group: String,
    /// Source topic.
    pub topic: String,
    /// Total attempts made by this group.
    pub attempts: u32,
    /// Bounded diagnostic supplied by the worker.
    pub reason: String,
    /// Dead-letter time in Unix milliseconds.
    pub dead_lettered_at: u64,
    /// Original publication time.
    pub published_at: u64,
    /// Original payload.
    pub payload: Value,
}
