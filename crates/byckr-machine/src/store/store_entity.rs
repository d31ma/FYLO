use byckr_engine::ReadDocument;
use serde_json::{Map, Value};

use super::store_failure::StoreFailure;
use crate::MachineError;

/// A Store entity: the document's fields with its id under the request's id
/// field. The stored document never holds the id field (`api/store/v1`,
/// "Documents and ids").
pub(crate) struct StoreEntity;

impl StoreEntity {
    pub(crate) fn from_record(record: &ReadDocument, id_field: &str) -> Value {
        Self::from_fields(
            &record.metadata.id,
            record.document.fields().clone(),
            id_field,
        )
    }

    pub(crate) fn from_fields(id: &str, mut fields: Map<String, Value>, id_field: &str) -> Value {
        fields.insert(id_field.to_owned(), Value::String(id.to_owned()));
        Value::Object(fields)
    }

    /// The entity of the document `id`, or `null` when it does not exist.
    pub(crate) fn or_null(id: &str, fields: Option<Map<String, Value>>, id_field: &str) -> Value {
        fields.map_or(Value::Null, |fields| {
            Self::from_fields(id, fields, id_field)
        })
    }

    pub(crate) fn list(records: &[ReadDocument], id_field: &str) -> Value {
        let entities = records
            .iter()
            .map(|record| Self::from_record(record, id_field))
            .collect();
        Value::Array(entities)
    }

    /// The entity's id (`None` when it is missing or `null`) and its fields
    /// without the id field.
    pub(crate) fn split(
        entity: &Map<String, Value>,
        id_field: &str,
    ) -> Result<(Option<String>, Map<String, Value>), MachineError> {
        let id = Self::id(entity.get(id_field), id_field)?;
        let mut fields = entity.clone();
        fields.remove(id_field);
        Ok((id, fields))
    }

    fn id(value: Option<&Value>, id_field: &str) -> Result<Option<String>, MachineError> {
        match value {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(id)) => Ok(Some(id.clone())),
            Some(_) => Err(StoreFailure::bad_request(format!(
                "the entity's \"{id_field}\" must be a string id or null"
            ))),
        }
    }
}
