use serde_json::{Map, Value};

use crate::{QueryError, QueryErrorCode};

/// Checks shared by every JSON object of the Store contract.
pub(crate) struct StoreFields;

impl StoreFields {
    pub(crate) fn object_or_empty<'a>(
        value: Option<&'a Value>,
        empty: &'a Map<String, Value>,
        what: &str,
    ) -> Result<&'a Map<String, Value>, QueryError> {
        match value {
            None | Some(Value::Null) => Ok(empty),
            Some(value) => Self::object(value, what),
        }
    }

    pub(crate) fn object<'a>(
        value: &'a Value,
        what: &str,
    ) -> Result<&'a Map<String, Value>, QueryError> {
        value
            .as_object()
            .ok_or_else(|| Self::invalid(&format!("{what} must be an object")))
    }

    pub(crate) fn require_known_keys(
        object: &Map<String, Value>,
        keys: &[&str],
        what: &str,
    ) -> Result<(), QueryError> {
        let unknown = object.keys().find(|key| !keys.contains(&key.as_str()));
        match unknown {
            Some(key) => Err(Self::invalid(&format!("{what} has no field \"{key}\""))),
            None => Ok(()),
        }
    }

    pub(crate) fn name(
        object: &Map<String, Value>,
        key: &str,
        what: &str,
    ) -> Result<String, QueryError> {
        match object.get(key) {
            Some(Value::String(name)) if !name.is_empty() => Ok(name.clone()),
            _ => Err(Self::invalid(&format!(
                "{what} needs a non-empty string \"{key}\""
            ))),
        }
    }

    pub(crate) fn invalid(message: &str) -> QueryError {
        QueryError::new(QueryErrorCode::InvalidQuery, message)
    }
}
