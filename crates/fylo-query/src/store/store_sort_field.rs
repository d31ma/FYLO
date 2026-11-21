use serde_json::Value;

use super::store_fields::StoreFields;
use crate::QueryError;

/// One `{ field, direction }` entry of a Store query's `orderBy`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreSortField {
    field: String,
    descending: bool,
}

impl StoreSortField {
    const KEYS: [&'static str; 2] = ["field", "direction"];
    const WHAT: &'static str = "an orderBy entry";

    /// Parse and check one `orderBy` entry.
    ///
    /// # Errors
    ///
    /// `EQUERY_INVALID` for a malformed entry or an unknown direction.
    pub fn from_value(entry: &Value) -> Result<Self, QueryError> {
        let object = StoreFields::object(entry, Self::WHAT)?;
        StoreFields::require_known_keys(object, &Self::KEYS, Self::WHAT)?;
        let field = StoreFields::name(object, "field", Self::WHAT)?;
        let descending = Self::parse_direction(object.get("direction"))?;
        Ok(Self { field, descending })
    }

    /// The field to sort by.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Whether larger values come first.
    #[must_use]
    pub const fn descending(&self) -> bool {
        self.descending
    }

    fn parse_direction(direction: Option<&Value>) -> Result<bool, QueryError> {
        match direction.map(Value::as_str) {
            None | Some(Some("asc")) => Ok(false),
            Some(Some("desc")) => Ok(true),
            _ => Err(StoreFields::invalid(
                "an orderBy direction must be \"asc\" or \"desc\"",
            )),
        }
    }
}
