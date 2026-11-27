/// What one compaction of a segment collection did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CompactionReport {
    /// Sealed segments replaced by the compacted one.
    pub segments_compacted: usize,
    /// Live documents copied into the compacted segment.
    pub documents_copied: usize,
    /// Bytes the replaced segments held.
    pub bytes_before: u64,
    /// Bytes of the compacted segment.
    pub bytes_after: u64,
}
