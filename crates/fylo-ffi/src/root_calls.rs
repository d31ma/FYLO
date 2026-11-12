use std::path::Path;
use std::sync::Arc;

use fylo::{Document, Query, Root};
use serde_json::{Value, json};

use crate::call_failure::CallFailure;
use crate::call_outcome::CallOutcome;
use crate::open_settings::OpenSettings;
use crate::root_entry::RootEntry;
use crate::root_table::RootTable;

/// The root calls behind the C entry points, in safe Rust. Results have the
/// machine protocol's shapes (`putData`, `getDoc`, `findDocs`, `delDoc`).
pub(crate) struct RootCalls;

impl RootCalls {
    pub(crate) fn open(path: &str, options: &str) -> Result<u64, CallFailure> {
        let settings = OpenSettings::parse(options)?;
        let options = settings.prepare(Path::new(path))?;
        let root = Root::open(path, options)?;
        Ok(RootTable::shared().insert(root))
    }

    pub(crate) fn close(handle: u64) -> CallOutcome {
        let entry = RootTable::shared().remove(handle);
        Self::close_entry(entry)
    }

    pub(crate) fn create_collection(handle: u64, name: &str) -> CallOutcome {
        Self::with_root(handle, |root| {
            root.create_collection(name)?;
            Ok(json!({ "collection": name, "kind": "document" }))
        })
    }

    pub(crate) fn put(handle: u64, collection: &str, document: &str) -> CallOutcome {
        Self::with_root(handle, |root| {
            let document = Self::document(document)?;
            Ok(Value::String(root.collection(collection).put(document)?))
        })
    }

    pub(crate) fn get(handle: u64, collection: &str, id: &str) -> CallOutcome {
        Self::with_entry(handle, |root| {
            let found = root.collection(collection).get(id);
            Self::found(id, found)
        })
    }

    pub(crate) fn delete(handle: u64, collection: &str, id: &str) -> CallOutcome {
        Self::with_root(handle, |root| {
            root.collection(collection).delete(id)?;
            Ok(json!({ "deleted": true }))
        })
    }

    pub(crate) fn find(handle: u64, collection: &str, query: &str) -> CallOutcome {
        Self::with_root(handle, |root| {
            let query = Query::from_json(&serde_json::from_str(query)?)?;
            let rows = root.collection(collection).find(&query)?;
            Ok(Value::Object(rows.into_iter().map(Self::row).collect()))
        })
    }

    fn with_root(
        handle: u64,
        call: impl FnOnce(&Root) -> Result<Value, CallFailure>,
    ) -> CallOutcome {
        Self::with_entry(handle, |root| CallOutcome::from_result(call(root)))
    }

    fn with_entry(handle: u64, call: impl FnOnce(&Root) -> CallOutcome) -> CallOutcome {
        match RootTable::shared().find(handle) {
            Some(entry) => entry.with_root(call),
            None => CallOutcome::Closed,
        }
    }

    fn close_entry(entry: Option<Arc<RootEntry>>) -> CallOutcome {
        match entry.and_then(|entry| entry.take()) {
            Some(root) => CallOutcome::from_result(Self::closed(root)),
            None => CallOutcome::Closed,
        }
    }

    fn closed(root: Root) -> Result<Value, CallFailure> {
        root.close()?;
        Ok(json!({}))
    }

    fn found(id: &str, found: Result<Option<Document>, fylo::Error>) -> CallOutcome {
        match found {
            Ok(Some(document)) => CallOutcome::Done(json!({ id: Value::Object(document) })),
            Ok(None) => CallOutcome::NotFound,
            Err(error) => CallOutcome::failed(error.into()),
        }
    }

    fn document(text: &str) -> Result<Document, CallFailure> {
        match serde_json::from_str::<Value>(text)? {
            Value::Object(document) => Ok(document),
            _ => Err(CallFailure::bad_request("a document must be a JSON object")),
        }
    }

    fn row((id, document): (String, Document)) -> (String, Value) {
        (id, Value::Object(document))
    }
}
