use std::cmp::Ordering;

use serde_json::Value;

use crate::{FieldValue, match_operand, matches_like};

/// The Store's strict value rules (`api/store/v1`, "Equality"): a value
/// never equals or orders against a value of another type.
pub(crate) struct StoreComparison;

impl StoreComparison {
    pub(crate) fn equal(actual: Option<&Value>, expected: &Value) -> bool {
        match (actual, expected) {
            (None | Some(Value::Null), Value::Null) => true,
            (Some(Value::Number(left)), Value::Number(right)) => left.as_f64() == right.as_f64(),
            (Some(Value::String(left)), Value::String(right)) => left == right,
            (Some(Value::Bool(left)), Value::Bool(right)) => left == right,
            _ => false,
        }
    }

    pub(crate) fn order(actual: Option<&Value>, expected: &Value) -> Option<Ordering> {
        match (actual, expected) {
            (Some(Value::Number(left)), Value::Number(right)) => {
                left.as_f64()?.partial_cmp(&right.as_f64()?)
            }
            (Some(Value::String(left)), Value::String(right)) => Some(left.cmp(right)),
            _ => None,
        }
    }

    pub(crate) fn not_equal(actual: Option<&Value>, expected: &Value) -> bool {
        let present = !Self::is_null(actual);
        present && !Self::equal(actual, expected)
    }

    pub(crate) fn is_null(actual: Option<&Value>) -> bool {
        matches!(actual, None | Some(Value::Null))
    }

    pub(crate) fn any_equal(actual: Option<&Value>, expected: &Value) -> bool {
        let values = expected.as_array().map_or(&[][..], Vec::as_slice);
        values.iter().any(|value| Self::equal(actual, value))
    }

    pub(crate) fn like(actual: Option<&Value>, pattern: &Value) -> bool {
        match (actual, pattern) {
            (Some(Value::String(text)), Value::String(pattern)) => matches_like(text, pattern),
            _ => false,
        }
    }

    pub(crate) fn words(actual: Option<&Value>, text: &Value) -> bool {
        let value = actual.map_or(FieldValue::Missing, FieldValue::Present);
        match_operand(value, text)
    }
}
