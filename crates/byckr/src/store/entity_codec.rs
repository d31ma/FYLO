use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::Error;

/// Turns a typed entity into stored fields and back (`api/store/v1`,
/// "Documents and ids"): the id lives under the id field of the entity and
/// never in the stored document.
pub(crate) struct EntityCodec;

impl EntityCodec {
    const BAD_ENTITY: &'static str = "EBADREQUEST";

    /// The entity's id (`None` when it is missing or `null`) and its fields
    /// without the id field.
    pub(crate) fn fields<T: Serialize>(
        entity: &T,
        id_field: &str,
    ) -> Result<(Option<String>, Map<String, Value>), Error> {
        let mut fields = Self::object(entity)?;
        let id = Self::id(fields.remove(id_field), id_field)?;
        Ok((id, fields))
    }

    /// The typed entity of the document `id`.
    pub(crate) fn entity<T: DeserializeOwned>(
        id: &str,
        mut fields: Map<String, Value>,
        id_field: &str,
    ) -> Result<T, Error> {
        fields.insert(id_field.to_owned(), Value::String(id.to_owned()));
        serde_json::from_value(Value::Object(fields)).map_err(|error| {
            Error::new(
                Self::BAD_ENTITY,
                format!("document {id} does not fit the entity type: {error}"),
            )
        })
    }

    fn object<T: Serialize>(entity: &T) -> Result<Map<String, Value>, Error> {
        match serde_json::to_value(entity) {
            Ok(Value::Object(fields)) => Ok(fields),
            Ok(_) => Err(Error::new(
                Self::BAD_ENTITY,
                "an entity must serialize to a JSON object",
            )),
            Err(error) => Err(Error::new(Self::BAD_ENTITY, error.to_string())),
        }
    }

    fn id(value: Option<Value>, id_field: &str) -> Result<Option<String>, Error> {
        match value {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(id)) => Ok(Some(id)),
            Some(_) => Err(Error::new(
                Self::BAD_ENTITY,
                format!("the entity's \"{id_field}\" must be a string id or null"),
            )),
        }
    }
}
