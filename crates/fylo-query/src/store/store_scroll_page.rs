use serde_json::{Map, Value};

use super::store_fields::StoreFields;
use super::store_page_size::StorePageSize;
use crate::QueryError;

/// A cursor page `{ size, cursor, maxSize }` of `storeScroll`. The first
/// page has no cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreScrollPage {
    size: usize,
    cursor: Option<String>,
}

impl StoreScrollPage {
    const KEYS: [&'static str; 3] = ["size", "cursor", "maxSize"];

    /// Parse and check a scroll page; `None` or `null` is the first page of
    /// the default size.
    ///
    /// # Errors
    ///
    /// `EPAGESIZE` for a size out of range, `EQUERY_INVALID` for a
    /// malformed page.
    pub fn from_value(page: Option<&Value>) -> Result<Self, QueryError> {
        let empty = Map::new();
        let object = StoreFields::object_or_empty(page, &empty, "a page")?;
        StoreFields::require_known_keys(object, &Self::KEYS, "a page")?;
        let size = StorePageSize::from_page(object)?;
        let cursor = Self::parse_cursor(object.get("cursor"))?;
        Ok(Self { size, cursor })
    }

    /// A page of `size` documents after `cursor`.
    #[must_use]
    pub const fn new(size: usize, cursor: Option<String>) -> Self {
        Self { size, cursor }
    }

    /// The most documents the page holds.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// The cursor the page resumes after, `None` on the first page.
    #[must_use]
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    fn parse_cursor(value: Option<&Value>) -> Result<Option<String>, QueryError> {
        match value {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(cursor)) => Ok(Some(cursor.clone())),
            Some(_) => Err(StoreFields::invalid(
                "a page's \"cursor\" must be a string or null",
            )),
        }
    }
}
