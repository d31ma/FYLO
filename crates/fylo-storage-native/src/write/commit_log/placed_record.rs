/// Where an appended commit record landed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PlacedRecord {
    /// The segment's identity: a document segment's number.
    pub(crate) segment: u64,
    pub(crate) offset: u64,
    pub(crate) length: u32,
    pub(crate) lsn: u64,
}
