use std::collections::BinaryHeap;

use byckr_query::{StorePosition, StoreQuery, StoreRow};

use super::store_sink::StoreSink;
use crate::EngineError;

/// Keeps the first `limit` matching documents in the query's order, after
/// an optional cursor position. It holds only positions (sort values and
/// ids), never documents, so its memory grows with `limit`, not with the
/// collection.
pub(crate) struct StoreRanking {
    limit: usize,
    after: Option<StorePosition>,
    best: BinaryHeap<StorePosition>,
}

impl StoreRanking {
    pub(crate) fn new(limit: usize, after: Option<StorePosition>) -> Self {
        Self {
            limit,
            after,
            best: BinaryHeap::new(),
        }
    }

    /// The kept positions, first in order first.
    pub(crate) fn into_sorted(self) -> Vec<StorePosition> {
        self.best.into_sorted_vec()
    }

    fn is_after_cursor(&self, position: &StorePosition) -> bool {
        self.after.as_ref().is_none_or(|after| position > after)
    }

    fn keep(&mut self, position: StorePosition) {
        self.best.push(position);
        if self.best.len() > self.limit {
            self.best.pop();
        }
    }
}

impl StoreSink for StoreRanking {
    fn offer(&mut self, query: &StoreQuery, row: &StoreRow<'_>) -> Result<bool, EngineError> {
        let position = query.position(row).map_err(EngineError::query)?;
        if self.is_after_cursor(&position) {
            self.keep(position);
        }
        Ok(false)
    }
}
