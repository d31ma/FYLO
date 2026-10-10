use serde::Serialize;

/// Queue depth and consumer-group state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStats {
    /// Immutable messages published to the topic.
    pub published: usize,
    /// Messages currently claimable by the selected group.
    pub available: usize,
    /// Messages waiting for their publication or retry delay.
    pub delayed: usize,
    /// Messages with active visibility leases.
    pub in_flight: usize,
    /// Messages durably acknowledged or dead-lettered for the group.
    pub retired: usize,
    /// Group-specific dead letters.
    pub dead_lettered: usize,
}
