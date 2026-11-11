use std::fmt::Write;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::index::{encode_field_path, encode_uri_component, numeric_value, sortable_float64};
use crate::sort_position::SortPosition;
use crate::{IndexKeyRange, QueryError, QueryErrorCode, normalize_field_path, value_by_path};

/// The `$sort` of a query: one field and a direction, served from the
/// field's sortable numeric keys, with a keyset cursor (ADR 0023 sections 5
/// and 6).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SortOrder {
    field: String,
    descending: bool,
    digest: String,
}

impl SortOrder {
    const CURSOR_FORMAT: &'static str = "fys1";
    const ASCENDING: &'static str = "asc";
    const DESCENDING: &'static str = "desc";
    const CURSOR_PARTS: usize = 5;
    const DIGEST_BYTES: usize = 8;
    const SORTABLE_HEX_DIGITS: usize = 16;

    /// The `$sort` of a decoded query, or `None` when it has none.
    pub(crate) fn parse(query: &Map<String, Value>) -> Result<Option<Self>, QueryError> {
        let Some(sort) = query.get("$sort") else {
            return Ok(None);
        };
        let (field, direction) = Self::single_entry(sort)?;
        let descending = Self::is_descending(direction)?;
        Ok(Some(Self {
            field: normalize_field_path(field),
            descending,
            digest: Self::digest(query),
        }))
    }

    /// The slash-separated sort field.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Whether larger values come first.
    #[must_use]
    pub const fn descending(&self) -> bool {
        self.descending
    }

    /// The index keys that hold this order: the field's `n` keys ascending,
    /// or its `nr` keys, whose ascending order is descending numeric order.
    #[must_use]
    pub fn key_prefix(&self) -> String {
        let kind = if self.descending { "nr" } else { "n" };
        format!("{}/{kind}/", encode_field_path(&self.field))
    }

    /// The keys to walk: all of the order's keys, or those after `after`.
    #[must_use]
    pub fn range_after(&self, after: Option<&SortPosition>) -> IndexKeyRange {
        let prefix = self.key_prefix();
        let whole = IndexKeyRange::prefix(&prefix);
        let Some(after) = after else {
            return whole;
        };
        let mut start = after.key(&prefix).into_bytes();
        start.push(0);
        IndexKeyRange::new(start, whole.end().to_vec())
    }

    /// The position a key of this order names.
    #[must_use]
    pub fn position_of_key(&self, key: &[u8]) -> Option<SortPosition> {
        let rest = key.strip_prefix(self.key_prefix().as_bytes())?;
        let rest = std::str::from_utf8(rest).ok()?;
        let (value, id) = rest.split_once('/')?;
        SortPosition::decoded(value, id)
    }

    /// The value of `document` that places it in this order: its smallest
    /// sortable key, which is its smallest number ascending and its largest
    /// descending. `None` for a document without a numeric value.
    #[must_use]
    pub fn placing_value(&self, document: &Map<String, Value>) -> Option<String> {
        let value = value_by_path(document, &self.field)?;
        let values = match value {
            Value::Array(items) => items.as_slice(),
            other => std::slice::from_ref(other),
        };
        values.iter().filter_map(|item| self.sortable(item)).min()
    }

    /// The cursor naming `position` under this order and query.
    #[must_use]
    pub fn cursor(&self, position: &SortPosition) -> String {
        format!(
            "{}.{}.{}.{}.{}",
            Self::CURSOR_FORMAT,
            self.direction_letter(),
            position.value(),
            encode_uri_component(position.id()),
            self.digest
        )
    }

    /// The position a cursor names, checked against this order and query.
    ///
    /// # Errors
    ///
    /// `EINVALIDCURSOR` for a cursor that does not parse or belongs to a
    /// different filter, sort field or direction.
    pub fn parse_cursor(&self, cursor: &str) -> Result<SortPosition, QueryError> {
        let parts: Vec<&str> = cursor.split('.').collect();
        if parts.len() != Self::CURSOR_PARTS || parts[0] != Self::CURSOR_FORMAT {
            return Err(Self::invalid_cursor("the cursor does not parse"));
        }
        if parts[1] != self.direction_letter() || parts[4] != self.digest {
            return Err(Self::invalid_cursor(
                "the cursor belongs to a different query; restart from the first page",
            ));
        }
        Self::require_sortable_hex(parts[2])?;
        SortPosition::decoded(parts[2], parts[3])
            .ok_or_else(|| Self::invalid_cursor("the cursor's document id does not parse"))
    }

    fn sortable(&self, value: &Value) -> Option<String> {
        let sortable = sortable_float64(numeric_value(value)?);
        if !self.descending {
            return Some(sortable);
        }
        let ascending = u64::from_str_radix(&sortable, 16).ok()?;
        Some(format!("{:016x}", u64::MAX - ascending))
    }

    fn direction_letter(&self) -> &'static str {
        if self.descending { "d" } else { "a" }
    }

    fn single_entry(sort: &Value) -> Result<(&String, &Value), QueryError> {
        let entries = sort
            .as_object()
            .filter(|entries| entries.len() == 1)
            .ok_or_else(|| Self::invalid_sort("$sort must be an object with exactly one field"))?;
        entries
            .iter()
            .next()
            .ok_or_else(|| Self::invalid_sort("$sort must name a field"))
    }

    fn is_descending(direction: &Value) -> Result<bool, QueryError> {
        match direction.as_str() {
            Some(Self::ASCENDING) => Ok(false),
            Some(Self::DESCENDING) => Ok(true),
            _ => Err(Self::invalid_sort(
                "$sort direction must be \"asc\" or \"desc\"",
            )),
        }
    }

    fn digest(query: &Map<String, Value>) -> String {
        let mut scope = Map::new();
        for field in ["$ops", "$created", "$updated", "$sort"] {
            scope.insert(
                field.into(),
                query.get(field).cloned().unwrap_or(Value::Null),
            );
        }
        let encoded = serde_json::to_vec(&Value::Object(scope)).unwrap_or_default();
        let digest = Sha256::digest(encoded);
        let mut hex = String::with_capacity(Self::DIGEST_BYTES * 2);
        for byte in &digest[..Self::DIGEST_BYTES] {
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    }

    fn require_sortable_hex(value: &str) -> Result<(), QueryError> {
        let valid = value.len() == Self::SORTABLE_HEX_DIGITS
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if valid {
            return Ok(());
        }
        Err(Self::invalid_cursor(
            "the cursor's sort value does not parse",
        ))
    }

    fn invalid_sort(message: &str) -> QueryError {
        QueryError::new(QueryErrorCode::InvalidQuery, message)
    }

    fn invalid_cursor(message: &str) -> QueryError {
        QueryError::new(QueryErrorCode::InvalidCursor, message)
    }
}
