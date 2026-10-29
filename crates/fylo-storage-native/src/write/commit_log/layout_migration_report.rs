use crate::CollectionLayout;

/// What a layout migration moved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutMigrationReport {
    /// The collection migrated.
    pub collection: String,
    /// The layout it has now.
    pub layout: CollectionLayout,
    /// Live documents carried over.
    pub documents: usize,
    /// Soft-deleted documents carried over.
    pub deleted: usize,
}
