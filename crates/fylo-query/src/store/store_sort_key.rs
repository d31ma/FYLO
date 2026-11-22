use std::cmp::Ordering;

use super::store_sort_value::StoreSortValue;

/// One sort value with the direction of its `orderBy` entry.
#[derive(Clone, Debug)]
pub struct StoreSortKey {
    value: StoreSortValue,
    descending: bool,
}

impl StoreSortKey {
    /// A key from a value and its direction.
    #[must_use]
    pub const fn new(value: StoreSortValue, descending: bool) -> Self {
        Self { value, descending }
    }

    /// The value.
    #[must_use]
    pub const fn value(&self) -> &StoreSortValue {
        &self.value
    }

    /// The order of two keys of the same `orderBy` entry, in that entry's
    /// direction.
    #[must_use]
    pub fn compare(&self, other: &Self) -> Ordering {
        let ascending = self.value.compare(&other.value);
        if self.descending {
            return ascending.reverse();
        }
        ascending
    }
}
