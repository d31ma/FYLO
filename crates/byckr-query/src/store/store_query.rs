use serde_json::{Map, Value, json};

use super::store_cursor::StoreCursor;
use super::store_fields::StoreFields;
use super::store_position::StorePosition;
use super::store_predicate::StorePredicate;
use super::store_row::StoreRow;
use super::store_sort_field::StoreSortField;
use super::store_sort_key::StoreSortKey;
use super::store_sort_value::StoreSortValue;
use crate::{QueryError, QueryErrorCode};

/// A checked Store query (`api/store/v1`, "Queries"): `where` predicates
/// joined by AND, an optional `anyOf` (OR between lists, AND inside), and up
/// to four `orderBy` entries, with the id ascending always last.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreQuery {
    all_of: Vec<StorePredicate>,
    any_of: Vec<Vec<StorePredicate>>,
    order: Vec<StoreSortField>,
    id_field: String,
    canonical: Value,
}

impl StoreQuery {
    /// The id field when a request names none.
    pub const DEFAULT_ID_FIELD: &'static str = "id";
    const MAX_PREDICATES: usize = 8;
    const MAX_SORT_FIELDS: usize = 4;
    const MAX_ANY_OF_LISTS: usize = 8;
    const MAX_INPUT_BYTES: usize = 64 * 1024;
    const KEYS: [&'static str; 3] = ["where", "anyOf", "orderBy"];

    /// Parse and check a query. `None` or `null` is the query that matches
    /// every document, ordered by id.
    ///
    /// # Errors
    ///
    /// `EQUERY_INPUT_SIZE` above 64 KiB, `EQUERYLIMIT` above a count limit,
    /// `EQUERY_INVALID` for any other malformed part.
    pub fn from_value(query: Option<&Value>, id_field: &str) -> Result<Self, QueryError> {
        let empty = Map::new();
        let object = StoreFields::object_or_empty(query, &empty, "a Store query")?;
        Self::require_size(object)?;
        StoreFields::require_known_keys(object, &Self::KEYS, "a Store query")?;
        let all_of = Self::parse_predicates(object.get("where"), "where")?;
        let any_of = Self::parse_any_of(object.get("anyOf"))?;
        let order = Self::parse_order(object.get("orderBy"))?;
        Self::require_predicate_count(&all_of, &any_of)?;
        Ok(Self {
            all_of,
            any_of,
            order,
            id_field: id_field.to_owned(),
            canonical: Self::canonical(object, id_field),
        })
    }

    /// The predicates every document must pass.
    #[must_use]
    pub fn all_of(&self) -> &[StorePredicate] {
        &self.all_of
    }

    /// The `anyOf` lists; a document must pass every predicate of at least
    /// one of them. Empty when the query has no `anyOf`.
    #[must_use]
    pub fn any_of(&self) -> &[Vec<StorePredicate>] {
        &self.any_of
    }

    /// The `orderBy` entries.
    #[must_use]
    pub fn order(&self) -> &[StoreSortField] {
        &self.order
    }

    /// The field that holds the document id.
    #[must_use]
    pub fn id_field(&self) -> &str {
        &self.id_field
    }

    /// Whether a document passes the query.
    #[must_use]
    pub fn matches(&self, row: &StoreRow<'_>) -> bool {
        let passes_all = self.passes(&self.all_of, row);
        passes_all && self.passes_any_of(row)
    }

    /// Where a document stands in the query's order.
    ///
    /// # Errors
    ///
    /// `EQUERY_INVALID` when a sort field holds an object or an array.
    pub fn position(&self, row: &StoreRow<'_>) -> Result<StorePosition, QueryError> {
        let keys = self
            .order
            .iter()
            .map(|entry| self.sort_key(entry, row))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(StorePosition::new(keys, row.id().to_owned()))
    }

    /// The cursor that resumes after `position`, tied to this query and to
    /// `scope` (the collection and namespace).
    #[must_use]
    pub fn cursor(&self, position: &StorePosition, scope: &str) -> String {
        StoreCursor::encode(position, &self.digest(scope))
    }

    /// The position a cursor names.
    ///
    /// # Errors
    ///
    /// `ECURSOR` for a cursor that does not parse or belongs to another
    /// query or scope.
    pub fn cursor_position(&self, cursor: &str, scope: &str) -> Result<StorePosition, QueryError> {
        StoreCursor::decode(cursor, &self.digest(scope), &self.order)
    }

    fn digest(&self, scope: &str) -> String {
        StoreCursor::digest(&json!({ "scope": scope, "query": self.canonical }))
    }

    fn sort_key(
        &self,
        entry: &StoreSortField,
        row: &StoreRow<'_>,
    ) -> Result<StoreSortKey, QueryError> {
        let value = row.value(entry.field(), &self.id_field);
        let sort_value = StoreSortValue::from_field(entry.field(), value)?;
        Ok(StoreSortKey::new(sort_value, entry.descending()))
    }

    fn passes_any_of(&self, row: &StoreRow<'_>) -> bool {
        if self.any_of.is_empty() {
            return true;
        }
        self.any_of.iter().any(|list| self.passes(list, row))
    }

    fn passes(&self, predicates: &[StorePredicate], row: &StoreRow<'_>) -> bool {
        predicates
            .iter()
            .all(|predicate| predicate.matches(row.value(predicate.field(), &self.id_field)))
    }

    fn require_size(object: &Map<String, Value>) -> Result<(), QueryError> {
        let size = serde_json::to_vec(object).map_or(usize::MAX, |bytes| bytes.len());
        if size <= Self::MAX_INPUT_BYTES {
            return Ok(());
        }
        Err(QueryError::new(
            QueryErrorCode::InputTooLarge,
            format!(
                "the Store query holds {size} bytes; the limit is {}",
                Self::MAX_INPUT_BYTES
            ),
        ))
    }

    fn parse_predicates(
        list: Option<&Value>,
        name: &str,
    ) -> Result<Vec<StorePredicate>, QueryError> {
        Self::list(list, name)?
            .iter()
            .map(StorePredicate::from_value)
            .collect()
    }

    fn parse_any_of(lists: Option<&Value>) -> Result<Vec<Vec<StorePredicate>>, QueryError> {
        let lists = Self::list(lists, "anyOf")?;
        Self::require_at_most(lists.len(), Self::MAX_ANY_OF_LISTS, "anyOf lists")?;
        lists
            .iter()
            .map(|list| Self::parse_predicates(Some(list), "anyOf"))
            .collect()
    }

    fn parse_order(entries: Option<&Value>) -> Result<Vec<StoreSortField>, QueryError> {
        let entries = Self::list(entries, "orderBy")?;
        Self::require_at_most(entries.len(), Self::MAX_SORT_FIELDS, "orderBy entries")?;
        entries.iter().map(StoreSortField::from_value).collect()
    }

    fn list<'a>(value: Option<&'a Value>, name: &str) -> Result<&'a [Value], QueryError> {
        match value {
            None | Some(Value::Null) => Ok(&[]),
            Some(Value::Array(items)) => Ok(items),
            Some(_) => Err(StoreFields::invalid(&format!("\"{name}\" must be a list"))),
        }
    }

    fn require_predicate_count(
        all_of: &[StorePredicate],
        any_of: &[Vec<StorePredicate>],
    ) -> Result<(), QueryError> {
        let count = all_of.len() + any_of.iter().map(Vec::len).sum::<usize>();
        Self::require_at_most(count, Self::MAX_PREDICATES, "predicates")
    }

    fn require_at_most(count: usize, limit: usize, what: &str) -> Result<(), QueryError> {
        if count <= limit {
            return Ok(());
        }
        Err(QueryError::new(
            QueryErrorCode::QueryLimit,
            format!("the Store query has {count} {what}; the limit is {limit}"),
        ))
    }

    fn canonical(object: &Map<String, Value>, id_field: &str) -> Value {
        json!({
            "idField": id_field,
            "where": object.get("where").cloned().unwrap_or(Value::Null),
            "anyOf": object.get("anyOf").cloned().unwrap_or(Value::Null),
            "orderBy": object.get("orderBy").cloned().unwrap_or(Value::Null),
        })
    }
}
