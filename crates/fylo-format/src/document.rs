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
