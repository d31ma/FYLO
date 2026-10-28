use std::mem::size_of;

use fylo_query::IndexSnapshot;
use serde_json::{Map, Value};

use super::collection_key::CollectionKey;
use super::resident_collection::ResidentCollection;
use crate::ReadDocument;

/// Conservative estimates of the heap an entry keeps alive.
///
/// They count every value node, string and map slot, plus a fixed cost per
/// entry for its handles and the clock queue, so the budget bounds real
/// memory rather than input bytes.
pub(crate) struct ResidentSize;

impl ResidentSize {
    /// The `Arc`, entry, map bucket and clock slot behind each entry.
    const ENTRY_OVERHEAD: usize = 128;
    /// The hash and index an ordered map keeps per field.
    const MAP_SLOT_OVERHEAD: usize = 16;

    pub(crate) fn header(key: &CollectionKey, collection: &ResidentCollection) -> usize {
        Self::ENTRY_OVERHEAD
            + key.heap_bytes()
            + size_of::<ResidentCollection>()
            + Self::strings(collection.encrypted_fields())
    }

    pub(crate) fn snapshot(key: &CollectionKey, snapshot: &IndexSnapshot) -> usize {
        Self::ENTRY_OVERHEAD + key.heap_bytes() + snapshot.as_bytes().len()
    }

    pub(crate) fn identifiers(key: &CollectionKey, identifiers: &[String]) -> usize {
        Self::ENTRY_OVERHEAD + key.heap_bytes() + Self::strings(identifiers)
    }

    pub(crate) fn document(key: &CollectionKey, identifier: &str, record: &ReadDocument) -> usize {
        Self::ENTRY_OVERHEAD
            + key.heap_bytes()
            + 2 * identifier.len()
            + size_of::<ReadDocument>()
            + Self::object(record.document.fields())
    }

    fn strings(values: &[String]) -> usize {
        let mut total = 0;
        for value in values {
            total += size_of::<String>() + value.capacity();
        }
        total
    }

    fn value(value: &Value) -> usize {
        size_of::<Value>() + Self::payload(value)
    }

    fn payload(value: &Value) -> usize {
        match value {
            Value::String(text) => text.capacity(),
            Value::Array(items) => Self::array(items),
            Value::Object(fields) => Self::object(fields),
            Value::Null | Value::Bool(_) | Value::Number(_) => 0,
        }
    }

    fn array(items: &[Value]) -> usize {
        let mut total = 0;
        for item in items {
            total += Self::value(item);
        }
        total
    }

    fn object(fields: &Map<String, Value>) -> usize {
        let mut total = 0;
        for (name, value) in fields {
            total += size_of::<String>() + name.capacity() + Self::MAP_SLOT_OVERHEAD;
            total += Self::value(value);
        }
        total
    }
}
