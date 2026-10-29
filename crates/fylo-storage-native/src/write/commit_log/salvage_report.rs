/// What `fylo salvage` recovered from a damaged segment collection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SalvageReport {
    /// Live and deleted documents written to the output tree.
    pub recovered: usize,
    /// Byte ranges no record could be read from, as `segment:start-end`.
    pub damaged: Vec<String>,
    /// Documents whose newest version the location table names could not
    /// be read. An older version may have been recovered instead.
    pub lost: Vec<String>,
    /// Ordinals of the schema versions recovered from schema records,
    /// written to `.fylo-schemas/<ordinal>.json` under the output tree in
    /// the directory registry's file format (ADR 0017 section 3).
    pub schema_versions: Vec<u32>,
}
