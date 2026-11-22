use fylo_query::{StoreQuery, StoreRow};

use super::store_sink::StoreSink;
use crate::EngineError;

/// Ends the scan at the first matching document.
#[derive(Default)]
pub(crate) struct StoreExistence {
    found: bool,
}

impl StoreExistence {
    pub(crate) const fn found(&self) -> bool {
        self.found
    }
}

impl StoreSink for StoreExistence {
    fn offer(&mut self, _query: &StoreQuery, _row: &StoreRow<'_>) -> Result<bool, EngineError> {
        self.found = true;
        Ok(true)
    }
}
