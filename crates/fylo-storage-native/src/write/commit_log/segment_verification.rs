/// What `fylo verify` found in a segment collection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SegmentVerification {
    /// Segment files checked.
    pub segments: usize,
    /// Records read.
    pub records: usize,
    /// Live and deleted documents the location table names.
    pub documents: usize,
    /// Schema versions the schema records hold (ADR 0017 section 3).
    pub schema_versions: usize,
    /// One line per defect: a damaged byte range, a body or schema version
    /// whose digest does not match, a location that disagrees with a
    /// rebuild, or a schemas snapshot that does.
    pub defects: Vec<String>,
}
