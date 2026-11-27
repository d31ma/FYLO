use serde::Serialize;

/// Acknowledgement result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueAckResult {
    /// The message is durably retired for this group.
    pub acknowledged: bool,
    /// The same successful receipt was acknowledged recently.
    pub duplicate: bool,
}
