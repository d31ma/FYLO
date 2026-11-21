use serde_json::{Map, Value};

/// One document change a Store write makes, planned against the committed
/// documents and any open transaction before it is written or held.
#[derive(Clone, Debug, PartialEq)]
pub enum StoreChange {
    /// Write the document `id` with `fields`; `existed` says whether it
    /// existed before the write.
    Put {
        /// The document's TTID.
        id: String,
        /// The document's fields after the write.
        fields: Map<String, Value>,
        /// Whether the document existed before the write.
        existed: bool,
    },
    /// Delete the documents `ids`, which exist before the write.
    Delete {
        /// The TTIDs to delete.
        ids: Vec<String>,
    },
}

impl StoreChange {
    /// A put of the document `id`.
    #[must_use]
    pub fn put(id: &str, fields: Map<String, Value>, existed: bool) -> Self {
        Self::Put {
            id: id.to_owned(),
            fields,
            existed,
        }
    }

    /// A deletion of the documents `ids`.
    #[must_use]
    pub const fn delete(ids: Vec<String>) -> Self {
        Self::Delete { ids }
    }

    /// How many documents the change deletes.
    #[must_use]
    pub fn deleted(&self) -> usize {
        match self {
            Self::Put { .. } => 0,
            Self::Delete { ids } => ids.len(),
        }
    }
}
