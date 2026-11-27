use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use byckr_engine::{StoreChange, StoreOverlay, StorePending};
use byckr_storage_native::CollectionWrites;
use serde_json::Value;

use super::store_failure::StoreFailure;
use crate::MachineError;

/// One open Store transaction of a machine session (`api/store/v1`,
/// "Transactions"): the namespace its begin frame named, the root and schema
/// folder of that frame, its time limit, and the writes it holds, per
/// collection, until commit. A transaction over several collections of the
/// one namespace commits all of them or none (ADR 0035, Amendment on several
/// collections; ADR 0013, Amendment 3).
pub(crate) struct StoreTransaction {
    namespace: String,
    /// The collection its `transactionBegin` frame named; reserved until the
    /// transaction ends, even before it is written.
    begin: String,
    target: Value,
    deadline: Instant,
    /// Collections in the order of their first write.
    order: Vec<String>,
    /// Writes per collection, keyed by name.
    pending: BTreeMap<String, StorePending>,
    /// Writes and document bytes over every collection, against the shared
    /// limits of 1,000 writes and 8 MiB.
    writes: usize,
    bytes: usize,
}

impl StoreTransaction {
    /// A transaction on `namespace`, reserving `begin`, for the root and
    /// schema folder that `target` names (the `root` and `schemaDir` fields
    /// of its first frame).
    pub(crate) fn new(namespace: &str, begin: &str, target: Value, timeout: Duration) -> Self {
        Self {
            namespace: namespace.to_owned(),
            begin: begin.to_owned(),
            target,
            deadline: Instant::now() + timeout,
            order: Vec::new(),
            pending: BTreeMap::new(),
            writes: 0,
            bytes: 0,
        }
    }

    pub(crate) fn namespace(&self) -> &str {
        &self.namespace
    }

    /// A request frame that names the transaction's root and schema folder.
    pub(crate) const fn target(&self) -> &Value {
        &self.target
    }

    /// How many writes the transaction holds, over every collection.
    pub(crate) const fn writes(&self) -> usize {
        self.writes
    }

    /// The transaction's writes over a collection: its overlay, or `empty`
    /// when the transaction has not written it yet.
    pub(crate) fn overlay<'a>(
        &'a self,
        collection: &str,
        empty: &'a StoreOverlay,
    ) -> &'a StoreOverlay {
        self.pending
            .get(collection)
            .map_or(empty, |pending| pending.overlay())
    }

    /// Whether the transaction has reserved or written `collection`.
    pub(crate) fn holds(&self, collection: &str) -> bool {
        collection == self.begin || self.pending.contains_key(collection)
    }

    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// Hold one planned change to `collection`, keeping the transaction's
    /// shared write and byte limits.
    ///
    /// # Errors
    ///
    /// `ETXNLIMIT` when the transaction would pass a limit; the caller then
    /// rolls it back.
    pub(crate) fn hold(
        &mut self,
        collection: &str,
        change: &StoreChange,
    ) -> Result<(), MachineError> {
        let writes = self.writes + change.write_count();
        let bytes = self.bytes.saturating_add(change.byte_size());
        Self::require_within(writes, bytes)?;
        if !self.order.iter().any(|name| name == collection) {
            self.order.push(collection.to_owned());
        }
        let pending = self.pending.entry(collection.to_owned()).or_default();
        pending
            .hold(change)
            .map_err(|error| StoreFailure::engine(&error))?;
        self.writes = writes;
        self.bytes = bytes;
        Ok(())
    }

    /// The writes that commit the transaction, one batch per written
    /// collection, in the order of each collection's first write.
    pub(crate) fn collection_writes(&self) -> Vec<CollectionWrites> {
        self.order
            .iter()
            .filter_map(|name| {
                let writes = self.pending.get(name)?.document_writes();
                (!writes.is_empty()).then(|| CollectionWrites {
                    collection: name.clone(),
                    writes,
                })
            })
            .collect()
    }

    fn require_within(writes: usize, bytes: usize) -> Result<(), MachineError> {
        if writes <= StorePending::MAX_WRITES && bytes <= StorePending::MAX_BYTES {
            return Ok(());
        }
        Err(StoreFailure::limit())
    }
}
