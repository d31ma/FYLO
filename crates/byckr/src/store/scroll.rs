/// One cursor page of a Store query: its entities in order, and the cursor
/// of the next page, or `None` on the last page.
#[derive(Clone, Debug, PartialEq)]
pub struct Scroll<T> {
    /// The page's entities.
    pub content: Vec<T>,
    /// The cursor that resumes after the last entity of `content`.
    pub next_cursor: Option<String>,
}
