use serde_json::{Map, Number, Value as Json};
use turso_core::{NonNan, Numeric, Value};

/// One SQL value, as bound to a statement or read from a row.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue {
    /// SQL `NULL`.
    Null,
    /// A 64-bit integer.
    Integer(i64),
    /// A finite 64-bit float.
    Real(f64),
    /// UTF-8 text.
    Text(String),
    /// Raw bytes.
    Blob(Vec<u8>),
}

impl SqlValue {
    pub(crate) fn to_turso(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Integer(number) => Value::Numeric(Numeric::Integer(*number)),
            Self::Real(number) => real(*number),
            Self::Text(text) => Value::from_text(text.clone()),
            Self::Blob(bytes) => Value::from_slice(bytes).unwrap_or(Value::Null),
        }
    }

    pub(crate) fn from_turso(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Numeric(Numeric::Integer(number)) => Self::Integer(*number),
            Value::Numeric(Numeric::Float(number)) => Self::Real(f64::from(*number)),
            Value::Text(text) => Self::Text(text.as_str().to_owned()),
            Value::Blob(_) => Self::Blob(value.to_blob().unwrap_or_default().to_vec()),
        }
    }
}

impl From<i64> for SqlValue {
    fn from(number: i64) -> Self {
        Self::Integer(number)
    }
}

impl From<f64> for SqlValue {
    fn from(number: f64) -> Self {
        Self::Real(number)
    }
}

impl From<&str> for SqlValue {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for SqlValue {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

pub(crate) fn real(number: f64) -> Value {
    NonNan::new(number).map_or(Value::Null, |number| Value::Numeric(Numeric::Float(number)))
}

/// A document field as a SQL value: a scalar as itself, `true` and `false`
/// as 1 and 0, and an object or array as its JSON text.
pub(crate) fn from_json(value: &Json) -> Value {
    match value {
        Json::Null => Value::Null,
        Json::Bool(flag) => Value::Numeric(Numeric::Integer(i64::from(*flag))),
        Json::Number(number) => number.as_i64().map_or_else(
            || real(number.as_f64().unwrap_or(f64::NAN)),
            |integer| Value::Numeric(Numeric::Integer(integer)),
        ),
        Json::String(text) => Value::from_text(text.clone()),
        Json::Array(_) | Json::Object(_) => Value::from_text(value.to_string()),
    }
}

/// A SQL value as a document field. Text stays text; a blob has no JSON
/// form.
pub(crate) fn to_json(value: &Value) -> Option<Json> {
    match value {
        Value::Null => Some(Json::Null),
        Value::Numeric(Numeric::Integer(number)) => Some(Json::from(*number)),
        Value::Numeric(Numeric::Float(number)) => {
            Number::from_f64(f64::from(*number)).map(Json::Number)
        }
        Value::Text(text) => Some(Json::String(text.as_str().to_owned())),
        Value::Blob(_) => None,
    }
}

/// The `doc` column's text as a document body.
pub(crate) fn document_of(value: &Value) -> Result<Map<String, Json>, String> {
    match value {
        Value::Null => Ok(Map::new()),
        Value::Text(text) => match serde_json::from_str(text.as_str()) {
            Ok(Json::Object(fields)) => Ok(fields),
            _ => Err("doc must be the text of a JSON object".to_owned()),
        },
        _ => Err("doc must be the text of a JSON object".to_owned()),
    }
}
