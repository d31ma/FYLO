use serde_json::{Map, Value};

use super::store_fields::StoreFields;
use super::store_page_size::StorePageSize;
use crate::QueryError;

/// An offset page `{ size, offset, maxSize }` of `storeFind` and
/// `storeFindAll`. There is no bound on the offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorePage {
    size: usize,
    offset: usize,
}

impl StorePage {
    const KEYS: [&'static str; 3] = ["size", "offset", "maxSize"];

    /// Parse and check a page; `None` or `null` is the first page of the
    /// default size.
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
        let offset = StorePageSize::count(object.get("offset"), "offset")?.unwrap_or_default();
        Ok(Self { size, offset })
    }

    /// A page of `size` documents after skipping `offset`.
    #[must_use]
    pub const fn new(size: usize, offset: usize) -> Self {
        Self { size, offset }
    }

    /// The most documents the page holds.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// How many matching documents come before the page.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// How many of the first documents in order the page needs to know.
    #[must_use]
    pub const fn end(&self) -> usize {
        self.offset.saturating_add(self.size)
    }
}
