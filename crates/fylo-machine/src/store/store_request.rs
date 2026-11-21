use fylo_query::{StorePage, StoreQuery, StoreScrollPage};
use serde_json::{Map, Value};

use super::store_failure::StoreFailure;
use crate::{MachineError, require_string};

/// The fields every Store operation shares (`api/store/v1`, "Common request
/// fields"), read from one request frame.
pub(crate) struct StoreRequest<'a> {
    request: &'a Value,
    collection: &'a str,
    id_field: String,
}

impl<'a> StoreRequest<'a> {
    pub(crate) fn parse(request: &'a Value) -> Result<Self, MachineError> {
        let collection = require_string(request, "collection")?;
        let id_field = Self::parse_id_field(request)?;
        Ok(Self {
            request,
            collection,
            id_field,
        })
    }

    pub(crate) const fn frame(&self) -> &'a Value {
        self.request
    }

    pub(crate) const fn collection(&self) -> &'a str {
        self.collection
    }

    pub(crate) fn id_field(&self) -> &str {
        &self.id_field
    }

    pub(crate) fn id(&self) -> Result<&'a str, MachineError> {
        require_string(self.request, "id")
    }

    pub(crate) fn query(&self) -> Result<StoreQuery, MachineError> {
        StoreQuery::from_value(self.request.get("query"), &self.id_field)
            .map_err(|error| StoreFailure::query(&error))
    }

    pub(crate) fn page(&self) -> Result<StorePage, MachineError> {
        StorePage::from_value(self.request.get("page")).map_err(|error| StoreFailure::page(&error))
    }

    pub(crate) fn scroll_page(&self) -> Result<StoreScrollPage, MachineError> {
        StoreScrollPage::from_value(self.request.get("page"))
            .map_err(|error| StoreFailure::page(&error))
    }

    pub(crate) fn object(&self, field: &str) -> Result<&'a Map<String, Value>, MachineError> {
        self.request
            .get(field)
            .and_then(Value::as_object)
            .ok_or_else(|| {
                StoreFailure::bad_request(format!(
                    "machine request field \"{field}\" must be an object"
                ))
            })
    }

    /// The `transaction` handle the request names, if any.
    pub(crate) fn transaction(&self) -> Result<Option<&'a str>, MachineError> {
        match self.request.get("transaction") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(handle)) if !handle.is_empty() => Ok(Some(handle)),
            Some(_) => Err(StoreFailure::bad_request(
                "machine request field \"transaction\" must be a non-empty string",
            )),
        }
    }

    /// A frame that names only this request's root and schema folder.
    pub(crate) fn target(&self) -> Value {
        Self::target_of(self.request)
    }

    pub(crate) fn target_of(request: &Value) -> Value {
        let mut target = Map::new();
        for field in ["root", "schemaDir"] {
            if let Some(value) = request.get(field) {
                target.insert(field.to_owned(), value.clone());
            }
        }
        Value::Object(target)
    }

    pub(crate) fn flag(&self, field: &str) -> Result<bool, MachineError> {
        match self.request.get(field) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(_) => Err(StoreFailure::bad_request(format!(
                "machine request field \"{field}\" must be a boolean"
            ))),
        }
    }

    fn parse_id_field(request: &Value) -> Result<String, MachineError> {
        match request.get("idField") {
            None | Some(Value::Null) => Ok(StoreQuery::DEFAULT_ID_FIELD.to_owned()),
            Some(Value::String(field)) if !field.is_empty() => Ok(field.clone()),
            Some(_) => Err(StoreFailure::bad_request(
                "machine request field \"idField\" must be a non-empty string",
            )),
        }
    }
}
