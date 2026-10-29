/// What `fylo verify` found in a segment collection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SegmentVerification {
    /// Segment files checked.
    pub segments: usize,
    /// Records read.
    pub records: usize,
    /// Live and deleted documents the location table names.
    pub documents: usize,
    /// One line per defect: a damaged byte range, a body whose digest does
    /// not match, or a location that disagrees with a rebuild.
    pub defects: Vec<String>,
}
