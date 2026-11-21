use std::cmp::Ordering;

use super::store_sort_key::StoreSortKey;

/// Where a document stands in a Store query's order: its sort keys, then
/// its id ascending as the last tie-breaker.
#[derive(Clone, Debug)]
pub struct StorePosition {
    keys: Vec<StoreSortKey>,
    id: String,
}

impl StorePosition {
    /// A position from a document's sort keys and its id.
    #[must_use]
    pub const fn new(keys: Vec<StoreSortKey>, id: String) -> Self {
        Self { keys, id }
    }

    /// The document id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The sort keys, in `orderBy` order.
    #[must_use]
    pub fn keys(&self) -> &[StoreSortKey] {
        &self.keys
    }

    fn compare_keys(&self, other: &Self) -> Ordering {
        let pairs = self.keys.iter().zip(&other.keys);
        let first_difference = pairs
            .map(|(left, right)| left.compare(right))
            .find(|order| order.is_ne());
        first_difference.unwrap_or(Ordering::Equal)
    }
}

impl Ord for StorePosition {
    fn cmp(&self, other: &Self) -> Ordering {
        self.compare_keys(other)
            .then_with(|| self.id.cmp(&other.id))
    }
}

impl PartialOrd for StorePosition {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for StorePosition {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for StorePosition {}
