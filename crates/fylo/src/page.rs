use crate::Document;

/// One page of a sorted query: its documents in `$sort` order, and the
/// cursor that `$after` takes to read the next page (ADR 0023).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    /// `(id, document)` pairs in order.
    pub items: Vec<(String, Document)>,
    /// The next page's `$after` cursor, or `None` on the last page.
    pub next_cursor: Option<String>,
}
