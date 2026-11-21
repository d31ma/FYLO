use serde_json::{Map, Value};

use super::store_failure::StoreFailure;
use crate::MachineError;

/// One write of a `storeTransaction` batch (`api/store/v1`,
/// "Transactions"), turned into the frame of the single Store operation it
/// stands for.
pub(crate) struct StoreBatchWrite;

impl StoreBatchWrite {
    const SHARED_FIELDS: [&'static str; 5] =
        ["root", "schemaDir", "collection", "namespace", "idField"];

    /// The operation and frame of one write: `{ save: entity }`,
    /// `{ patch: { id, fields } }`, `{ deleteById: id }` or
    /// `{ delete: query }`.
    pub(crate) fn frame(
        batch: &Value,
        write: &Value,
    ) -> Result<(&'static str, Value), MachineError> {
        let (kind, body) = Self::single_entry(write)?;
        let mut frame = Self::shared(batch);
        let operation = Self::fill(&mut frame, kind, body)?;
        Ok((operation, Value::Object(frame)))
    }

    fn single_entry(write: &Value) -> Result<(&str, &Value), MachineError> {
        let entries = write.as_object().filter(|entries| entries.len() == 1);
        entries
            .and_then(|entries| entries.iter().next())
            .map(|(kind, body)| (kind.as_str(), body))
            .ok_or_else(|| {
                StoreFailure::bad_request(
                    "each transaction write must be an object with exactly one of save, patch, deleteById or delete",
                )
            })
    }

    fn shared(batch: &Value) -> Map<String, Value> {
        let mut frame = Map::new();
        for field in Self::SHARED_FIELDS {
            if let Some(value) = batch.get(field) {
                frame.insert(field.to_owned(), value.clone());
            }
        }
        frame
    }

    fn fill(
        frame: &mut Map<String, Value>,
        kind: &str,
        body: &Value,
    ) -> Result<&'static str, MachineError> {
        match kind {
            "save" => Ok(Self::with(frame, "storeSave", [("entity", body.clone())])),
            "deleteById" => Ok(Self::with(frame, "storeDeleteById", [("id", body.clone())])),
            "delete" => Ok(Self::with(frame, "storeDelete", [("query", body.clone())])),
            "patch" => Ok(Self::patch(frame, body)),
            other => Err(StoreFailure::bad_request(format!(
                "\"{other}\" is not a transaction write; use save, patch, deleteById or delete"
            ))),
        }
    }

    fn patch(frame: &mut Map<String, Value>, body: &Value) -> &'static str {
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let fields = body.get("fields").cloned().unwrap_or(Value::Null);
        Self::with(frame, "storePatch", [("id", id), ("fields", fields)])
    }

    fn with<const N: usize>(
        frame: &mut Map<String, Value>,
        operation: &'static str,
        fields: [(&str, Value); N],
    ) -> &'static str {
        for (name, value) in fields {
            frame.insert(name.to_owned(), value);
        }
        operation
    }
}
