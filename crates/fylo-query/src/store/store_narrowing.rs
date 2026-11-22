use serde_json::{Map, Value, json};

use super::store_operator::StoreOperator;
use super::store_predicate::StorePredicate;
use super::store_query::StoreQuery;
use crate::{DEFAULT_MAX_QUERIES, QueryLimits, StructuredQuery};

/// The structured query whose index candidates hold every document a Store
/// query can match, so the engine reads only those documents.
///
/// The Store's strict rules match a subset of what the structured query's
/// loose rules match, for the operators used here: `eq` with a string or a
/// number, and `lt`, `lte`, `gt`, `gte` with a number. So the candidates can
/// hold extra documents, never miss one, and the Store query still decides
/// each document. An `in` on its own becomes one `$eq` alternative per value.
pub struct StoreNarrowing;

impl StoreNarrowing {
    /// The narrowing query, or `None` when some branch of the Store query
    /// has no predicate the index can answer (the engine then reads every
    /// document).
    #[must_use]
    pub fn structured(query: &StoreQuery) -> Option<StructuredQuery> {
        let operations = Self::operations(query)?;
        Self::bounded(operations)
    }

    fn operations(query: &StoreQuery) -> Option<Vec<Value>> {
        let branches = Self::branches(query);
        let per_branch: Option<Vec<Vec<Value>>> = branches
            .iter()
            .map(|branch| Self::branch_operations(branch, query.id_field()))
            .collect();
        per_branch.map(|lists| lists.into_iter().flatten().collect())
    }

    fn branches(query: &StoreQuery) -> Vec<Vec<&StorePredicate>> {
        let all_of: Vec<&StorePredicate> = query.all_of().iter().collect();
        if query.any_of().is_empty() {
            return vec![all_of];
        }
        query
            .any_of()
            .iter()
            .map(|list| all_of.iter().copied().chain(list).collect())
            .collect()
    }

    fn branch_operations(branch: &[&StorePredicate], id_field: &str) -> Option<Vec<Value>> {
        let usable: Vec<&StorePredicate> = branch
            .iter()
            .copied()
            .filter(|predicate| Self::is_plain_field(predicate.field(), id_field))
            .collect();
        let ranges = Self::range_operation(&usable);
        if !ranges.is_empty() {
            return Some(vec![Value::Object(ranges)]);
        }
        Self::in_operations(&usable)
    }

    fn range_operation(predicates: &[&StorePredicate]) -> Map<String, Value> {
        let mut operation = Map::new();
        for predicate in predicates {
            Self::add_range(&mut operation, predicate);
        }
        operation
    }

    fn add_range(operation: &mut Map<String, Value>, predicate: &StorePredicate) {
        let Some(name) = Self::index_operator(predicate) else {
            return;
        };
        let operators = operation
            .entry(predicate.field().to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        operators[name] = predicate.value().clone();
    }

    fn index_operator(predicate: &StorePredicate) -> Option<&'static str> {
        let name = predicate.operator().index_operator()?;
        let value = predicate.value();
        let usable = value.is_number() || (name == "$eq" && value.is_string());
        usable.then_some(name)
    }

    fn in_operations(predicates: &[&StorePredicate]) -> Option<Vec<Value>> {
        let list = predicates
            .iter()
            .find(|predicate| Self::is_index_list(predicate))?;
        let values = list.value().as_array()?;
        Some(
            values
                .iter()
                .map(|value| json!({ list.field(): { "$eq": value } }))
                .collect(),
        )
    }

    fn is_index_list(predicate: &StorePredicate) -> bool {
        let is_in = predicate.operator() == StoreOperator::In;
        is_in && Self::all_indexable(predicate.value())
    }

    fn all_indexable(list: &Value) -> bool {
        let values = list.as_array().map_or(&[][..], Vec::as_slice);
        values
            .iter()
            .all(|value| value.is_number() || value.is_string())
    }

    fn is_plain_field(field: &str, id_field: &str) -> bool {
        let structured = field.contains(['.', '/', '$']);
        field != id_field && !structured
    }

    fn bounded(operations: Vec<Value>) -> Option<StructuredQuery> {
        if operations.is_empty() || operations.len() > DEFAULT_MAX_QUERIES {
            return None;
        }
        StructuredQuery::from_value(&json!({ "$ops": operations }), QueryLimits::default()).ok()
    }
}
