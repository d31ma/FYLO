use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use fylo_engine::StoreOverlay;
use fylo_storage_native::DocumentWrite;
use serde_json::{Map, Value};

use crate::MachineError;

/// One open Store transaction on one collection (`api/store/v1`,
/// "Transactions"): its writes, held in memory until commit, and whether
/// each document it touched existed before it began.
pub(crate) struct StoreTransaction {
    collection: String,
    target: Value,
    deadline: Instant,
    overlay: StoreOverlay,
    existed: BTreeMap<String, bool>,
    writes: usize,
    bytes: usize,
}

impl StoreTransaction {
    pub(crate) const MAX_WRITES: usize = 1000;
    pub(crate) const MAX_BYTES: usize = 8 * 1024 * 1024;
    pub(crate) const LIMIT: &'static str = "ETXNLIMIT";

    /// A transaction on `collection` of the root and schema folder that
    /// `target` names (the `root` and `schemaDir` fields of its first frame).
    pub(crate) fn new(collection: &str, target: Value, timeout: Duration) -> Self {
        Self {
            collection: collection.to_owned(),
            target,
            deadline: Instant::now() + timeout,
            overlay: StoreOverlay::default(),
            existed: BTreeMap::new(),
            writes: 0,
            bytes: 0,
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
        &self.overlay
    }

    pub(crate) const fn writes(&self) -> usize {
        self.writes
    }

    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// Hold the document `id` with `fields` after this write.
    pub(crate) fn record_put(
        &mut self,
        id: &str,
        fields: Map<String, Value>,
        existed: bool,
    ) -> Result<(), MachineError> {
        let size = serde_json::to_vec(&fields).map_or(usize::MAX, |bytes| bytes.len());
        self.count(size)?;
        self.existed.entry(id.to_owned()).or_insert(existed);
        self.overlay.put(id, fields);
        Ok(())
    }

    /// Hold the deletion of the document `id`.
    pub(crate) fn record_delete(&mut self, id: &str, existed: bool) -> Result<(), MachineError> {
        self.count(0)?;
        self.existed.entry(id.to_owned()).or_insert(existed);
        self.overlay.delete(id);
        Ok(())
    }

    /// The writes that commit this transaction: one per document it touched,
    /// from the document's state before the transaction to its state after.
    pub(crate) fn document_writes(&self) -> Vec<DocumentWrite> {
        self.overlay
            .entries()
            .filter_map(|(id, fields)| self.document_write(id, fields.as_ref()))
            .collect()
    }

    fn document_write(
        &self,
        id: &str,
        fields: Option<&Map<String, Value>>,
    ) -> Option<DocumentWrite> {
        let existed = self.existed.get(id).copied().unwrap_or(false);
        let identifier = id.to_owned();
        match (fields, existed) {
            (Some(fields), true) => Some(DocumentWrite::Replace {
                identifier,
                fields: fields.clone(),
            }),
            (Some(fields), false) => Some(DocumentWrite::Put {
                identifier,
                fields: fields.clone(),
            }),
            (None, true) => Some(DocumentWrite::Delete { identifier }),
            (None, false) => None,
        }
    }

    fn count(&mut self, size: usize) -> Result<(), MachineError> {
        let writes = self.writes + 1;
        let bytes = self.bytes.saturating_add(size);
        Self::require_within(writes, bytes)?;
        self.writes = writes;
        self.bytes = bytes;
        Ok(())
    }

    fn require_within(writes: usize, bytes: usize) -> Result<(), MachineError> {
        if writes <= Self::MAX_WRITES && bytes <= Self::MAX_BYTES {
            return Ok(());
        }
        Err(MachineError::new(
            Self::LIMIT,
            format!(
                "a transaction holds at most {} writes and {} bytes of documents; it was rolled back",
                Self::MAX_WRITES,
                Self::MAX_BYTES
            ),
        ))
    }
}
