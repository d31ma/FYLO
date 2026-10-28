use serde::Serialize;

/// A point-in-time view of a [`crate::HotSet`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotSetStatistics {
    /// The most bytes the hot set retains.
    pub budget_bytes: usize,
    /// Bytes retained now, as accounted against the budget.
    pub resident_bytes: usize,
    /// Collections with a resident header.
    pub collections: usize,
    /// Resident parsed documents.
    pub documents: usize,
    /// Lookups answered from memory.
    pub hits: u64,
    /// Lookups that had to read storage.
    pub misses: u64,
    /// Entries dropped to make room.
    pub evictions: u64,
    /// Entries not retained because they alone exceed the budget.
    pub refusals: u64,
    /// Times every entry was dropped because the root may have changed.
    pub clears: u64,
}
