use byckr_query::StoreQuery;
use serde_json::{Value, json};

use super::direction::Direction;
use crate::Error;

/// A Store query built in Rust (`api/store/v1`, "Queries"): predicates
/// joined by AND, and up to four sort fields, with the id ascending always
/// last.
///
/// ```
/// use byckr::store::{Direction, Query};
///
/// let open_first = Query::new()
///     .eq("status", "open")
///     .gte("priority", 3)
///     .order_by("priority", Direction::Descending);
/// # let _ = open_first;
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    predicates: Vec<Value>,
    order: Vec<Value>,
}

impl Query {
    /// The query that matches every document, ordered by id.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `field` equals `value` (numbers by value, strings exactly).
    #[must_use]
    pub fn eq(self, field: &str, value: impl Into<Value>) -> Self {
        self.with(field, "eq", Some(value.into()))
    }

    /// `field` is present, not `null`, and not equal to `value`.
    #[must_use]
    pub fn ne(self, field: &str, value: impl Into<Value>) -> Self {
        self.with(field, "ne", Some(value.into()))
    }

    /// `field` is less than `value` (a number or a string, same type only).
    #[must_use]
    pub fn lt(self, field: &str, value: impl Into<Value>) -> Self {
        self.with(field, "lt", Some(value.into()))
    }

    /// `field` is less than or equal to `value`.
    #[must_use]
    pub fn lte(self, field: &str, value: impl Into<Value>) -> Self {
        self.with(field, "lte", Some(value.into()))
    }

    /// `field` is greater than `value`.
    #[must_use]
    pub fn gt(self, field: &str, value: impl Into<Value>) -> Self {
        self.with(field, "gt", Some(value.into()))
    }

    /// `field` is greater than or equal to `value`.
    #[must_use]
    pub fn gte(self, field: &str, value: impl Into<Value>) -> Self {
        self.with(field, "gte", Some(value.into()))
    }

    /// `field` equals one of `values` (1 to 100 scalars).
    #[must_use]
    pub fn is_in(self, field: &str, values: Vec<Value>) -> Self {
        self.with(field, "in", Some(Value::Array(values)))
    }

    /// `field` is `null` or missing.
    #[must_use]
    pub fn is_null(self, field: &str) -> Self {
        self.with(field, "isNull", None)
    }

    /// `field` is present and not `null`.
    #[must_use]
    pub fn is_not_null(self, field: &str) -> Self {
        self.with(field, "isNotNull", None)
    }

    /// Sort by `field` in `direction`, after any earlier sort fields.
    #[must_use]
    pub fn order_by(mut self, field: &str, direction: Direction) -> Self {
        self.order
            .push(json!({ "field": field, "direction": direction.name() }));
        self
    }

    /// The checked engine query, with the id under `id_field`.
    pub(crate) fn checked(&self, id_field: &str) -> Result<StoreQuery, Error> {
        let query = json!({ "where": self.predicates, "orderBy": self.order });
        StoreQuery::from_value(Some(&query), id_field)
            .map_err(|error| Error::new(error.code().as_str(), error.message()))
    }

    fn with(mut self, field: &str, operator: &str, value: Option<Value>) -> Self {
        let mut predicate = json!({ "field": field, "operator": operator });
        if let Some(value) = value {
            predicate["value"] = value;
        }
        self.predicates.push(predicate);
        self
    }
}
