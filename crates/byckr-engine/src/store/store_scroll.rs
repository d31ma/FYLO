use crate::ReadDocument;

/// One `storeScroll` page: its documents in order, and the cursor of the
/// next page, or `None` on the last page.
#[derive(Debug, Default)]
pub struct StoreScroll {
    /// Matching documents in the query's order.
    pub content: Vec<ReadDocument>,
    /// The cursor that resumes after the last document of `content`.
    pub next_cursor: Option<String>,
}
