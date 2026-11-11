use fylo_query::{SortOrder, SortPosition};

use crate::ReadDocument;

/// One page of a sorted query: its documents in order, and the cursor that
/// resumes after the last one, or `None` on the last page (ADR 0023).
#[derive(Debug, Default)]
pub struct SortedPage {
    /// Matching documents in sort order.
    pub rows: Vec<ReadDocument>,
    /// The `$after` cursor of the next page, when there is one.
    pub next_cursor: Option<String>,
    limit: Option<usize>,
    last: Option<SortPosition>,
}

impl SortedPage {
    pub(crate) fn new(limit: Option<usize>) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    /// Add a matching document found at `position`. Returns true once the
    /// page is full and another match exists, which ends the walk with a
    /// cursor after the page's last document.
    pub(crate) fn offer(
        &mut self,
        record: Option<ReadDocument>,
        position: &SortPosition,
        sort: &SortOrder,
    ) -> bool {
        let Some(record) = record else {
            return false;
        };
        if self.is_full() {
            self.next_cursor = self.last.as_ref().map(|last| sort.cursor(last));
            return true;
        }
        self.rows.push(record);
        self.last = Some(position.clone());
        false
    }

    fn is_full(&self) -> bool {
        self.limit.is_some_and(|limit| self.rows.len() >= limit)
    }
}
