use serde_json::{Map, Value};

/// One document change a Store write makes, worked out against a view of
/// the collection before it is written to storage or held in a
/// transaction.
pub(crate) enum StoreChange {
    /// Write the document `id` with `fields`; `existed` says whether it
    /// existed in the view before the write.
    Put {
        id: String,
        fields: Map<String, Value>,
        existed: bool,
    },
    /// Delete the documents `ids`, which exist in the view.
    Delete { ids: Vec<String> },
}

impl StoreChange {
    pub(crate) fn put(id: &str, fields: Map<String, Value>, existed: bool) -> Self {
        Self::Put {
            id: id.to_owned(),
            fields,
            existed,
        }
    }

    pub(crate) const fn delete(ids: Vec<String>) -> Self {
        Self::Delete { ids }
    }
}
