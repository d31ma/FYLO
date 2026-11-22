use fylo_query::{StoreQuery, StoreRow};

use crate::EngineError;

/// What a Store scan does with each document that passes the query.
pub(crate) trait StoreSink {
    /// Take one matching document. Returns true to end the scan early.
    fn offer(&mut self, query: &StoreQuery, row: &StoreRow<'_>) -> Result<bool, EngineError>;
}
