/// The newest change of one document, as the change feed reports it
/// (ADR 0024).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentChange {
    /// The document's identifier.
    pub id: String,
    /// Whether the document's newest version is a deletion.
    pub deleted: bool,
    /// The log sequence number of the document's newest change.
    pub lsn: u64,
}
