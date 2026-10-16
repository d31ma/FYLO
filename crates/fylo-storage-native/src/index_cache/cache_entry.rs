use super::IndexInputs;
use fylo_query::IndexSnapshot;
use std::sync::Arc;

pub(super) struct CacheEntry {
    inputs: IndexInputs,
    snapshot: Arc<IndexSnapshot>,
}

impl CacheEntry {
    pub(super) fn new(inputs: IndexInputs, snapshot: Arc<IndexSnapshot>) -> Self {
        Self { inputs, snapshot }
    }

    pub(super) fn reuse(&self, inputs: &IndexInputs) -> Option<Arc<IndexSnapshot>> {
        self.snapshot_for_inputs(inputs)
    }

    fn snapshot_for_inputs(&self, inputs: &IndexInputs) -> Option<Arc<IndexSnapshot>> {
        if self.inputs == *inputs {
            return Some(Arc::clone(&self.snapshot));
        }
        None
    }
}
