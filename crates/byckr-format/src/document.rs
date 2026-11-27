use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::document_validator::DocumentValidator;
use crate::{DocumentLimits, FormatError, FormatErrorCode};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Document(Map<String, Value>);

impl Document {
    pub const FORMAT_V1: &str = "fylo.document.json.v1";

    /// # Errors
    /// Returns a format error for invalid JSON, document shape, or resource limits.
    pub fn parse(bytes: &[u8], limits: DocumentLimits) -> Result<Self, FormatError> {
        Self::check_size(bytes, limits)?;
        let value = serde_json::from_slice(bytes).map_err(|error| Self::parse_error(&error))?;
        Self::try_from_value(value, limits)
    }

    /// # Errors
    /// Returns a format error for invalid document shape or resource limits.
    pub fn try_from_value(value: Value, limits: DocumentLimits) -> Result<Self, FormatError> {
        let map = Self::object(value)?;
        DocumentValidator::new(limits).validate(&map)?;
        Ok(Self(map))
    }

    /// # Errors
    /// Returns a format error if JSON encoding fails.
    pub fn encode(&self) -> Result<Vec<u8>, FormatError> {
        serde_json::to_vec(&self.0).map_err(|error| Self::encode_error(&error))
    }

    #[must_use]
    pub const fn fields(&self) -> &Map<String, Value> {
        &self.0
    }

    #[must_use]
    pub fn into_fields(self) -> Map<String, Value> {
        self.0
    }

    /// A deep copy of `fields` that costs one stack frame per nesting level.
    ///
    /// `Map::clone` recurses through about nine frames per level, and a
    /// browser Worker's stack is small (Safari's smallest): copying a 56-level
    /// document that way ran out of it there. Copy documents with this.
    #[must_use]
    pub fn copy_fields(fields: &Map<String, Value>) -> Map<String, Value> {
        let mut copy = Map::with_capacity(fields.len());
        for (key, value) in fields {
            copy.insert(key.clone(), Self::copy_value(value));
        }
        copy
    }

    /// [`Document::copy_fields`] for any JSON value.
    #[must_use]
    pub fn copy_value(value: &Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(Self::copy_fields(fields)),
            Value::Array(items) => Value::Array(items.iter().map(Self::copy_value).collect()),
            scalar => scalar.clone(),
        }
    }

    fn check_size(bytes: &[u8], limits: DocumentLimits) -> Result<(), FormatError> {
        if bytes.len() > limits.max_bytes {
            return Err(FormatError::new(
                FormatErrorCode::DocumentTooLarge,
                format!(
                    "document contains {} bytes; limit is {}",
                    bytes.len(),
                    limits.max_bytes
                ),
            ));
        }
        Ok(())
    }

    fn object(value: Value) -> Result<Map<String, Value>, FormatError> {
        match value {
            Value::Object(map) => Ok(map),
            _ => Err(FormatError::new(
                FormatErrorCode::DocumentRoot,
                "FYLO document root must be a JSON object",
            )),
        }
    }

    fn parse_error(error: &serde_json::Error) -> FormatError {
        FormatError::new(FormatErrorCode::InvalidJson, error.to_string())
    }

    fn encode_error(error: &serde_json::Error) -> FormatError {
        FormatError::new(FormatErrorCode::Encode, error.to_string())
    }
}
