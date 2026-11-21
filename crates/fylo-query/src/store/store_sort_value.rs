use std::cmp::Ordering;

use serde_json::Value;

use crate::{QueryError, QueryErrorCode};

/// A document's value at one sort field, in the Store's cross-type order:
/// missing and `null` first, then booleans, numbers and strings
/// (`api/store/v1`, "Sort order").
#[derive(Clone, Debug)]
pub enum StoreSortValue {
    /// A missing field or `null`.
    Null,
    /// `false` or `true`.
    Boolean(bool),
    /// A finite number.
    Number(f64),
    /// A string, ordered by Unicode code points.
    Text(String),
}

impl StoreSortValue {
    /// The sort value of a field, `None` when the field is missing.
    ///
    /// # Errors
    ///
    /// `EQUERY_INVALID` for an object or an array, which cannot be a sort key.
    pub fn from_field(field: &str, value: Option<&Value>) -> Result<Self, QueryError> {
        match value {
            None | Some(Value::Null) => Ok(Self::Null),
            Some(Value::Bool(flag)) => Ok(Self::Boolean(*flag)),
            Some(Value::Number(number)) => Ok(Self::Number(number.as_f64().unwrap_or_default())),
            Some(Value::String(text)) => Ok(Self::Text(text.clone())),
            Some(Value::Array(_) | Value::Object(_)) => Err(QueryError::new(
                QueryErrorCode::InvalidQuery,
                format!(
                    "the field \"{field}\" holds an object or an array, which cannot be a sort key"
                ),
            )),
        }
    }

    /// The value as it is written in a cursor.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Boolean(flag) => Value::Bool(*flag),
            Self::Number(number) => Value::from(*number),
            Self::Text(text) => Value::String(text.clone()),
        }
    }

    /// The value a cursor wrote, or `None` when it is not a scalar.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<Self> {
        Self::from_field("", Some(value)).ok()
    }

    /// The order of two values, ascending.
    #[must_use]
    pub fn compare(&self, other: &Self) -> Ordering {
        let by_type = self.type_rank().cmp(&other.type_rank());
        by_type.then_with(|| self.compare_same_type(other))
    }

    fn compare_same_type(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Boolean(left), Self::Boolean(right)) => left.cmp(right),
            (Self::Number(left), Self::Number(right)) => {
                left.partial_cmp(right).unwrap_or(Ordering::Equal)
            }
            (Self::Text(left), Self::Text(right)) => left.cmp(right),
            _ => Ordering::Equal,
        }
    }

    const fn type_rank(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Boolean(_) => 1,
            Self::Number(_) => 2,
            Self::Text(_) => 3,
        }
    }
}
