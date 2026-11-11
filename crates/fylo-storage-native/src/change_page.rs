use crate::DocumentChange;

/// One page of a collection's change feed (ADR 0024).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangePage {
    /// The changes after the reader's position, oldest first.
    pub changes: Vec<DocumentChange>,
    /// Whether more changes follow the last one in this page.
    pub more: bool,
    /// The sequence number the collection gives its next change.
    pub next_lsn: u64,
    /// The collection's change lineage: a new value whenever its sequence
    /// numbers start again. `None` for a collection made before lineages.
    pub lineage: Option<String>,
}
