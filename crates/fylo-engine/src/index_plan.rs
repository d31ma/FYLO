use std::collections::BTreeSet;

use fylo_query::{IndexKeyRange, IndexSnapshot, StructuredQuery};

use crate::EngineError;
use crate::index_operation::{FieldRules, IndexOperation};

/// The documents a query can match, found in the prefix index: the union of
/// its operations' candidates.
///
/// A query is planned only when every operation can narrow its candidates.
/// Candidates can include extra documents, never miss one, so the query's
/// predicate still decides each document.
pub(super) struct IndexPlan {
    operations: Vec<IndexOperation>,
}

impl IndexPlan {
    pub(super) fn new(
        query: &StructuredQuery,
        encrypted: &[String],
        words: &[String],
    ) -> Option<Self> {
        let operations = Self::operations(query, &FieldRules { encrypted, words })?;
        Self::nonempty(operations)
    }

    pub(super) fn scan(&self, snapshot: &IndexSnapshot) -> Result<Vec<String>, EngineError> {
        let union = self.union(snapshot)?;
        Ok(union.into_iter().collect())
    }

    /// Every index key range the plan reads.
    pub(super) fn key_ranges(&self) -> Vec<IndexKeyRange> {
        self.operations
            .iter()
            .flat_map(IndexOperation::key_ranges)
            .cloned()
            .collect()
    }

    /// The top-level fields the plan reads from the index.
    pub(super) fn fields(&self) -> impl Iterator<Item = &str> {
        self.operations.iter().flat_map(IndexOperation::names)
    }

    fn operations(query: &StructuredQuery, rules: &FieldRules<'_>) -> Option<Vec<IndexOperation>> {
        query
            .operations()
            .iter()
            .map(|operation| IndexOperation::new(operation, rules))
            .collect()
    }

    fn nonempty(operations: Vec<IndexOperation>) -> Option<Self> {
        if operations.is_empty() {
            return None;
        }
        Some(Self { operations })
    }

    fn union(&self, snapshot: &IndexSnapshot) -> Result<BTreeSet<String>, EngineError> {
        let mut union = BTreeSet::new();
        for operation in &self.operations {
            union.extend(operation.scan(snapshot)?);
        }
        Ok(union)
    }
}
