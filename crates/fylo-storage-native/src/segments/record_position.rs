/// A record boundary in a segment collection: a segment number and a byte
/// offset in it, such as the end a published slot names.
///
/// Positions order by segment, then offset, which is the order commits
/// were appended in.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RecordPosition {
    pub(crate) segment: u64,
    pub(crate) offset: u64,
}

impl RecordPosition {
    /// Before every record of every segment: where a collection that has
    /// never written a location snapshot replays from.
    pub(crate) const START: Self = Self {
        segment: 0,
        offset: 0,
    };

    /// The end of the records `slot` publishes.
    pub(crate) fn published(slot: &super::SlotRecord) -> Self {
        Self {
            segment: slot.segment,
            offset: slot.length,
        }
    }
}
