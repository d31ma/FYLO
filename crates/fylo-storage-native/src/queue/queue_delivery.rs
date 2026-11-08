use serde::Serialize;
use serde_json::Value;

/// One leased delivery returned to a worker.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueDelivery {
    /// Message identifier.
    pub id: String,
    /// Topic supplied by the producer.
    pub topic: String,
    /// Original publication time in Unix milliseconds.
    pub published_at: u64,
    /// One-based delivery attempt for this consumer group.
    pub attempt: u32,
    /// Opaque receipt required by acknowledgement, rejection, and extension.
    pub receipt: String,
    /// Unix millisecond at which this claim becomes visible again.
    pub lease_expires_at: u64,
    /// Producer payload.
    pub payload: Value,
}
