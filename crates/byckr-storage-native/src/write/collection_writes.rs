use super::DocumentWrite;

/// The writes one transaction makes to one collection of a namespace.
///
/// A transaction lists its collections in the order of each one's first
/// write; they become `stable` in that order.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionWrites {
    /// The collection's name.
    pub collection: String,
    /// The writes, in order. Each identifier may appear once.
    pub writes: Vec<DocumentWrite>,
}
