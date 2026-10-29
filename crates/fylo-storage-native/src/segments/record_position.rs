/// A record boundary in a segment collection: a segment number and a byte
/// offset in it, such as the end a published slot names.
///
/// Positions order by segment, then offset, which is the order commits
/// were appended in.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "a medium without a location log is the first caller"
    )
)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RecordPosition {
    pub(crate) segment: u64,
    pub(crate) offset: u64,
}
