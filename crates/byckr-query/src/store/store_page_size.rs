use serde_json::{Map, Value};

use crate::{QueryError, QueryErrorCode};

/// A page's size: `size` (default 20) up to `maxSize` (default 100, never
/// above 1,000) (`api/store/v1`, "Pages").
pub(crate) struct StorePageSize;

impl StorePageSize {
    const DEFAULT_SIZE: usize = 20;
    const DEFAULT_MAX_SIZE: usize = 100;
    const CEILING: usize = 1000;

    pub(crate) fn from_page(page: &Map<String, Value>) -> Result<usize, QueryError> {
        let max_size = Self::max_size(page.get("maxSize"))?;
        let size = Self::count(page.get("size"), "size")?;
        Self::require_within(size.unwrap_or(Self::DEFAULT_SIZE.min(max_size)), max_size)
    }

    pub(crate) fn count(value: Option<&Value>, name: &str) -> Result<Option<usize>, QueryError> {
        match value {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(number)) => Self::whole(number.as_u64(), name).map(Some),
            Some(_) => Err(Self::invalid(name)),
        }
    }

    fn whole(number: Option<u64>, name: &str) -> Result<usize, QueryError> {
        let number = number.ok_or_else(|| Self::invalid(name))?;
        usize::try_from(number).map_err(|_| Self::invalid(name))
    }

    fn max_size(value: Option<&Value>) -> Result<usize, QueryError> {
        let max_size = Self::count(value, "maxSize")?.unwrap_or(Self::DEFAULT_MAX_SIZE);
        Self::require_within(max_size, Self::CEILING)
    }

    fn require_within(size: usize, limit: usize) -> Result<usize, QueryError> {
        if (1..=limit).contains(&size) {
            return Ok(size);
        }
        Err(QueryError::new(
            QueryErrorCode::PageSize,
            format!("a page size must be from 1 to {limit}; it is {size}"),
        ))
    }

    fn invalid(name: &str) -> QueryError {
        QueryError::new(
            QueryErrorCode::InvalidQuery,
            format!("the page's \"{name}\" must be a non-negative integer"),
        )
    }
}
