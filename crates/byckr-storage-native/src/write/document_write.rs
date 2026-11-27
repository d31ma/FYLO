use serde_json::{Map, Value};

/// One document change in a batch that commits as one log record.
#[derive(Clone, Debug, PartialEq)]
pub enum DocumentWrite {
    /// Create a document at an identifier that is not live.
    Put {
        /// The document's TTID.
        identifier: String,
        /// The document body.
        fields: Map<String, Value>,
    },
    /// Replace the body of a live document.
    Replace {
        /// The document's TTID.
        identifier: String,
        /// The new document body.
        fields: Map<String, Value>,
    },
    /// Delete a live document, keeping its tombstone.
    Delete {
        /// The document's TTID.
        identifier: String,
    },
}

impl DocumentWrite {
    /// The identifier this write changes.
    #[must_use]
    pub fn identifier(&self) -> &str {
        match self {
            Self::Put { identifier, .. }
            | Self::Replace { identifier, .. }
            | Self::Delete { identifier } => identifier,
        }
    }
}
