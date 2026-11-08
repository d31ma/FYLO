use super::compaction_report::CompactionReport;
use super::salvage_report::SalvageReport;

/// What `fylo compact` did to a `.vdbx` database.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DatabaseCompactionReport {
    /// Each collection's compaction, in directory order.
    pub collections: Vec<(String, CompactionReport)>,
    /// Extents a shrink moved nearer the front of the file.
    pub extents_moved: usize,
    /// Free extents a shrink punched a hole over, where the filesystem
    /// punches holes; zero elsewhere.
    pub extents_punched: u64,
    /// The file's length before the compaction.
    pub file_bytes_before: u64,
    /// The file's length after it.
    pub file_bytes_after: u64,
}

/// What `fylo salvage` recovered from a `.vdbx` database.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DatabaseSalvageReport {
    /// Each collection's salvage, in directory order.
    pub collections: Vec<(String, SalvageReport)>,
}
