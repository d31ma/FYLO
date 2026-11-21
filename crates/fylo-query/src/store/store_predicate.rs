use std::cmp::Ordering;

use serde_json::{Map, Value};

use super::store_comparison::StoreComparison;
use super::store_fields::StoreFields;
use super::store_operator::StoreOperator;
use crate::{QueryError, QueryErrorCode, match_terms};

/// One `{ field, operator, value }` entry of a Store query's `where`.
#[derive(Clone, Debug, PartialEq)]
pub struct StorePredicate {
    field: String,
    operator: StoreOperator,
    value: Value,
}

impl StorePredicate {
    const MAX_IN_VALUES: usize = 100;
    const KEYS: [&'static str; 3] = ["field", "operator", "value"];
    const WHAT: &'static str = "a Store predicate";

    /// Parse and check one predicate.
    ///
    /// # Errors
    ///
    /// `EQUERY_INVALID` for a malformed predicate or a value of the wrong
    /// type for its operator; `EQUERYLIMIT` for an `in` above 100 values.
    pub fn from_value(predicate: &Value) -> Result<Self, QueryError> {
        let object = StoreFields::object(predicate, Self::WHAT)?;
        StoreFields::require_known_keys(object, &Self::KEYS, Self::WHAT)?;
        let field = StoreFields::name(object, "field", Self::WHAT)?;
        let operator = Self::parse_operator(object)?;
        let value = object.get("value").cloned().unwrap_or(Value::Null);
        Self::check_value(operator, &value)?;
        Ok(Self {
            field,
            operator,
            value,
        })
    }

    /// The field this predicate tests.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// The operator.
    #[must_use]
    pub const fn operator(&self) -> StoreOperator {
        self.operator
    }

    /// The operand: a scalar, the list of an `in`, or `null`.
    #[must_use]
    pub const fn value(&self) -> &Value {
        &self.value
    }

    /// Whether `actual`, the field's value or `None` when it is missing,
    /// passes this predicate.
    #[must_use]
    pub fn matches(&self, actual: Option<&Value>) -> bool {
        let expected = &self.value;
        match self.operator {
            StoreOperator::Equal => StoreComparison::equal(actual, expected),
            StoreOperator::NotEqual => StoreComparison::not_equal(actual, expected),
            StoreOperator::LessThan => {
                StoreComparison::order(actual, expected).is_some_and(Ordering::is_lt)
            }
            StoreOperator::LessThanOrEqual => {
                StoreComparison::order(actual, expected).is_some_and(Ordering::is_le)
            }
            StoreOperator::GreaterThan => {
                StoreComparison::order(actual, expected).is_some_and(Ordering::is_gt)
            }
            StoreOperator::GreaterThanOrEqual => {
                StoreComparison::order(actual, expected).is_some_and(Ordering::is_ge)
            }
            StoreOperator::In => StoreComparison::any_equal(actual, expected),
            StoreOperator::IsNull => StoreComparison::is_null(actual),
            StoreOperator::IsNotNull => !StoreComparison::is_null(actual),
            StoreOperator::Like => StoreComparison::like(actual, expected),
            StoreOperator::Match => StoreComparison::words(actual, expected),
        }
    }

    fn parse_operator(object: &Map<String, Value>) -> Result<StoreOperator, QueryError> {
        let name = object
            .get("operator")
            .and_then(Value::as_str)
            .ok_or_else(|| StoreFields::invalid("a Store predicate needs a string \"operator\""))?;
        StoreOperator::parse(name)
    }

    fn check_value(operator: StoreOperator, value: &Value) -> Result<(), QueryError> {
        match operator {
            StoreOperator::Equal | StoreOperator::NotEqual => Self::require_scalar(operator, value),
            StoreOperator::LessThan
            | StoreOperator::LessThanOrEqual
            | StoreOperator::GreaterThan
            | StoreOperator::GreaterThanOrEqual => Self::require_ordered(operator, value),
            StoreOperator::In => Self::require_list(value),
            StoreOperator::IsNull | StoreOperator::IsNotNull => Self::require_null(operator, value),
            StoreOperator::Like => Self::require_text(operator, value),
            StoreOperator::Match => match_terms(value).map(|_| ()),
        }
    }

    fn require_scalar(operator: StoreOperator, value: &Value) -> Result<(), QueryError> {
        if Self::is_scalar(value) {
            return Ok(());
        }
        Err(Self::wrong_value(
            operator,
            "a string, number, boolean or null",
        ))
    }

    fn require_ordered(operator: StoreOperator, value: &Value) -> Result<(), QueryError> {
        if value.is_number() || value.is_string() {
            return Ok(());
        }
        Err(Self::wrong_value(operator, "a number or a string"))
    }

    fn require_text(operator: StoreOperator, value: &Value) -> Result<(), QueryError> {
        if value.is_string() {
            return Ok(());
        }
        Err(Self::wrong_value(operator, "a string"))
    }

    fn require_null(operator: StoreOperator, value: &Value) -> Result<(), QueryError> {
        if value.is_null() {
            return Ok(());
        }
        Err(Self::wrong_value(operator, "absent"))
    }

    fn require_list(value: &Value) -> Result<(), QueryError> {
        let values = Self::nonempty_list(value)?;
        Self::require_list_size(values)?;
        Self::require_scalar_items(values)
    }

    fn nonempty_list(value: &Value) -> Result<&Vec<Value>, QueryError> {
        match value {
            Value::Array(values) if !values.is_empty() => Ok(values),
            _ => Err(Self::wrong_value(
                StoreOperator::In,
                "a list of 1 to 100 scalars",
            )),
        }
    }

    fn require_list_size(values: &[Value]) -> Result<(), QueryError> {
        if values.len() <= Self::MAX_IN_VALUES {
            return Ok(());
        }
        Err(QueryError::new(
            QueryErrorCode::QueryLimit,
            format!(
                "an \"in\" predicate holds {} values; the limit is {}",
                values.len(),
                Self::MAX_IN_VALUES
            ),
        ))
    }

    fn require_scalar_items(values: &[Value]) -> Result<(), QueryError> {
        if values.iter().all(Self::is_scalar) {
            return Ok(());
        }
        Err(Self::wrong_value(StoreOperator::In, "a list of scalars"))
    }

    fn is_scalar(value: &Value) -> bool {
        !matches!(value, Value::Array(_) | Value::Object(_))
    }

    fn wrong_value(operator: StoreOperator, expected: &str) -> QueryError {
        StoreFields::invalid(&format!(
            "the value of a \"{}\" predicate must be {expected}",
            operator.name()
        ))
    }
}
