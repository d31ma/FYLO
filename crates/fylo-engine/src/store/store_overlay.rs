use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// The writes of an open Store transaction that are not committed yet
/// (`api/store/v1`, "Transactions"): for each document it touched, its
/// fields after the transaction, or `None` when the transaction deleted it.
/// A read with the transaction lays these over the committed documents, so
/// it sees the transaction's own writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoreOverlay {
    documents: BTreeMap<String, Option<Map<String, Value>>>,
}

impl StoreOverlay {
    /// Record the document `id` with `fields` after the transaction.
    pub fn put(&mut self, id: &str, fields: Map<String, Value>) {
        self.documents.insert(id.to_owned(), Some(fields));
    }

    /// Record that the transaction deleted the document `id`.
    pub fn delete(&mut self, id: &str) {
        self.documents.insert(id.to_owned(), None);
    }

    /// Whether the transaction touched the document `id`.
    #[must_use]
    pub fn touches(&self, id: &str) -> bool {
        self.documents.contains_key(id)
    }

    /// The document `id` after the transaction: `None` when the transaction
    /// did not touch it, `Some(None)` when it deleted it.
    #[must_use]
    pub fn state(&self, id: &str) -> Option<Option<&Map<String, Value>>> {
        self.documents.get(id).map(Option::as_ref)
    }

    /// Every document the transaction touched, in id order.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &Option<Map<String, Value>>)> {
        self.documents.iter()
    }

    /// The documents the transaction holds after it, in id order.
    pub fn present(&self) -> impl Iterator<Item = (&String, &Map<String, Value>)> {
        self.documents
            .iter()
            .filter_map(|(id, fields)| fields.as_ref().map(|fields| (id, fields)))
    }
}
