use std::collections::BTreeSet;

use fylo_query::{IndexSnapshot, StructuredQuery};

use crate::EngineError;
use crate::equality_operation::EqualityOperation;

pub(super) struct EqualityPlan {
    operations: Vec<EqualityOperation>,
}

impl EqualityPlan {
    pub(super) fn new(query: &StructuredQuery, encrypted: &[String]) -> Option<Self> {
        let operations = Self::operations(query, encrypted)?;
        Self::nonempty(operations)
    }

    pub(super) fn scan(&self, snapshot: &IndexSnapshot) -> Result<Vec<String>, EngineError> {
        let union = self.union(snapshot)?;
        Ok(union.into_iter().collect())
    }

    /// The top-level fields the plan reads from the index.
    pub(super) fn fields(&self) -> impl Iterator<Item = &str> {
        self.operations.iter().flat_map(EqualityOperation::names)
    }

    fn operations(query: &StructuredQuery, encrypted: &[String]) -> Option<Vec<EqualityOperation>> {
        query
            .operations()
            .iter()
            .map(|operation| EqualityOperation::new(operation, encrypted))
            .collect()
    }

    fn nonempty(operations: Vec<EqualityOperation>) -> Option<Self> {
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
