use fylo_query::{StoreQuery, StoreRow};

use super::store_sink::StoreSink;
use crate::EngineError;

/// Counts the matching documents.
#[derive(Default)]
pub(crate) struct StoreCounter {
    count: u64,
}

impl StoreCounter {
    pub(crate) const fn count(&self) -> u64 {
        self.count
    }
}

impl StoreSink for StoreCounter {
    fn offer(&mut self, _query: &StoreQuery, _row: &StoreRow<'_>) -> Result<bool, EngineError> {
        self.count += 1;
        Ok(false)
    }
}
