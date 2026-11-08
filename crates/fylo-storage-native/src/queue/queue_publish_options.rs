/// Options applied while publishing one immutable message.
#[derive(Clone, Debug, Default)]
pub struct QueuePublishOptions {
    /// Do not make the message claimable before this delay elapses.
    pub delay_ms: u64,
    /// Optional producer key that makes retries return the original message.
    pub idempotency_key: Option<String>,
}
