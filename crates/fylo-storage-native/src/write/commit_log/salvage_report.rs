/// What `fylo salvage` recovered from a damaged segment collection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SalvageReport {
    /// Live and deleted documents written to the output tree.
    pub recovered: usize,
    /// Byte ranges no record could be read from, as `segment:start-end`.
    pub damaged: Vec<String>,
    /// Documents whose newest version the location table names could not
    /// be read. An older version may have been recovered instead.
    ///
    /// A collection without a location log (a `.fxdb` container) builds
    /// its table from the records after its snapshot, and the table stops
    /// at the first it cannot read. Documents whose newest version lay in
    /// a damaged record there are named only when that record's manifest
    /// still reads; otherwise its byte range in `damaged` is all that is
    /// known of them.
    pub lost: Vec<String>,
}
