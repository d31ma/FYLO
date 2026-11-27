use serde_json::{Map, Value};

/// One document as a Store query sees it: its top-level fields, and its id
/// under the query's id field.
#[derive(Clone, Debug)]
pub struct StoreRow<'a> {
    id: Value,
    document: &'a Map<String, Value>,
}

impl<'a> StoreRow<'a> {
    /// A row for the document `id`.
    #[must_use]
    pub fn new(id: &str, document: &'a Map<String, Value>) -> Self {
        Self {
            id: Value::String(id.to_owned()),
            document,
        }
    }

    /// The document id.
    #[must_use]
    pub fn id(&self) -> &str {
        self.id.as_str().unwrap_or_default()
    }

    /// The value of a top-level field, or the id when `field` is the id
    /// field; `None` when the field is missing.
    #[must_use]
    pub fn value(&self, field: &str, id_field: &str) -> Option<&Value> {
        if field == id_field {
            return Some(&self.id);
        }
        self.document.get(field)
    }
}
