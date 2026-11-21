use crate::CollectionInspection;

/// A collection's inspection with the number of its live documents its
/// index refers to, read in one stable generation from the collection's
/// listing and its index, without reading any document
/// (`ReadOnlyEngine::inspect_indexed`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedInspection {
    /// What `ReadOnlyEngine::inspect` reports.
    pub inspection: CollectionInspection,
    /// Live documents (or raw files) at least one index key refers to.
    pub indexed_documents: usize,
}
