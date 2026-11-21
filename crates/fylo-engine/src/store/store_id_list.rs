use fylo_query::{StoreQuery, StoreRow};

use super::store_sink::StoreSink;
use crate::EngineError;

/// Collects the id of every matching document.
#[derive(Default)]
pub(crate) struct StoreIdList {
    ids: Vec<String>,
}

impl StoreIdList {
    pub(crate) fn into_ids(self) -> Vec<String> {
        self.ids
    }
}

impl StoreSink for StoreIdList {
    fn offer(&mut self, _query: &StoreQuery, row: &StoreRow<'_>) -> Result<bool, EngineError> {
        self.ids.push(row.id().to_owned());
        Ok(false)
    }
}
