use std::time::{Duration, Instant};

use fylo_engine::{StoreChange, StoreOverlay, StorePending};
use serde_json::Value;

use super::store_failure::StoreFailure;
use crate::MachineError;

/// One open Store transaction of a machine session (`api/store/v1`,
/// "Transactions"): its collection, the root its first frame named, its
/// time limit, and the writes it holds until commit.
pub(crate) struct StoreTransaction {
    collection: String,
    target: Value,
    deadline: Instant,
    pending: StorePending,
}

impl StoreTransaction {
    /// A transaction on `collection` of the root and schema folder that
    /// `target` names (the `root` and `schemaDir` fields of its first frame).
    pub(crate) fn new(collection: &str, target: Value, timeout: Duration) -> Self {
        Self {
            collection: collection.to_owned(),
            target,
            deadline: Instant::now() + timeout,
            pending: StorePending::default(),
        }
    }

    pub(crate) fn collection(&self) -> &str {
        &self.collection
    }

    /// A request frame that names the transaction's root and schema folder.
    pub(crate) const fn target(&self) -> &Value {
        &self.target
    }

    pub(crate) const fn overlay(&self) -> &StoreOverlay {
        self.pending.overlay()
    }

    pub(crate) const fn pending(&self) -> &StorePending {
        &self.pending
    }

    pub(crate) const fn writes(&self) -> usize {
        self.pending.writes()
    }

    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    pub(crate) fn hold(&mut self, change: &StoreChange) -> Result<(), MachineError> {
        self.pending
            .hold(change)
            .map_err(|error| StoreFailure::engine(&error))
    }
}
