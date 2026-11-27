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

    /// How many writes the change makes: one for a put, one per document
    /// deleted. This is what the transaction's 1,000-write limit counts.
    #[must_use]
    pub fn write_count(&self) -> usize {
        match self {
            Self::Put { .. } => 1,
            Self::Delete { ids } => ids.len(),
        }
    }

    /// The byte size of the documents the change writes, which the
    /// transaction's 8 MiB limit counts. A deletion writes no bytes; a put
    /// counts its serialized fields, or the maximum when serialization fails
    /// (so the write is refused rather than silently accepted).
    #[must_use]
    pub fn byte_size(&self) -> usize {
        match self {
            Self::Put { fields, .. } => {
                serde_json::to_vec(fields).map_or(usize::MAX, |bytes| bytes.len())
            }
            Self::Delete { .. } => 0,
        }
    }
}
