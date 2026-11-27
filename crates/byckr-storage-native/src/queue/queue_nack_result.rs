use serde::Serialize;

/// Negative-acknowledgement result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueNackResult {
    /// The message exhausted its attempts and entered the group DLQ.
    pub dead_lettered: bool,
    /// Earliest next claim time, absent after dead lettering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_at: Option<u64>,
}
