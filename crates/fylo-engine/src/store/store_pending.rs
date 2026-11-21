use std::collections::BTreeMap;

use fylo_storage_native::DocumentWrite;
use serde_json::{Map, Value};

use super::store_change::StoreChange;
use super::store_overlay::StoreOverlay;
use crate::{EngineError, EngineErrorCode};

/// The writes an open one-collection transaction holds until it commits
/// (`api/store/v1`, "Transactions"): the documents after each write, which
/// of them existed before the transaction, and its size against the limits
/// of 1,000 writes and 8 MiB of documents.
#[derive(Clone, Debug, Default)]
pub struct StorePending {
    overlay: StoreOverlay,
    existed: BTreeMap<String, bool>,
    writes: usize,
    bytes: usize,
}

impl StorePending {
    /// The most writes one transaction holds.
    pub const MAX_WRITES: usize = 1000;
    /// The most bytes of documents one transaction holds.
    pub const MAX_BYTES: usize = 8 * 1024 * 1024;

    /// The documents after the transaction's writes, for reads inside it.
    #[must_use]
    pub const fn overlay(&self) -> &StoreOverlay {
        &self.overlay
    }

    /// How many writes the transaction holds.
    #[must_use]
    pub const fn writes(&self) -> usize {
        self.writes
    }

    /// Hold one planned change.
    ///
    /// # Errors
    ///
    /// `ETXNLIMIT` when the transaction would pass a limit; the caller then
    /// rolls it back.
    pub fn hold(&mut self, change: &StoreChange) -> Result<(), EngineError> {
        match change {
            StoreChange::Put {
                id,
                fields,
                existed,
            } => self.hold_put(id, fields, *existed),
            StoreChange::Delete { ids } => ids.iter().try_for_each(|id| self.hold_delete(id)),
        }
    }

    /// The writes that commit the transaction: one per document it touched,
    /// from the document's state before it to its state after it.
    #[must_use]
    pub fn document_writes(&self) -> Vec<DocumentWrite> {
        self.overlay
            .entries()
            .filter_map(|(id, fields)| self.document_write(id, fields.as_ref()))
            .collect()
    }

    fn hold_put(
        &mut self,
        id: &str,
        fields: &Map<String, Value>,
        existed: bool,
    ) -> Result<(), EngineError> {
        let size = serde_json::to_vec(fields).map_or(usize::MAX, |bytes| bytes.len());
        self.count(size)?;
        self.existed.entry(id.to_owned()).or_insert(existed);
        self.overlay.put(id, fields.clone());
        Ok(())
    }

    /// A deleted document exists in the transaction's view, so when the
    /// transaction has not touched it yet, it existed before the transaction.
    fn hold_delete(&mut self, id: &str) -> Result<(), EngineError> {
        self.count(0)?;
        self.existed.entry(id.to_owned()).or_insert(true);
        self.overlay.delete(id);
        Ok(())
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

    fn count(&mut self, size: usize) -> Result<(), EngineError> {
        let writes = self.writes + 1;
        let bytes = self.bytes.saturating_add(size);
        Self::require_within(writes, bytes)?;
        self.writes = writes;
        self.bytes = bytes;
        Ok(())
    }

    fn require_within(writes: usize, bytes: usize) -> Result<(), EngineError> {
        if writes <= Self::MAX_WRITES && bytes <= Self::MAX_BYTES {
            return Ok(());
        }
        Err(EngineError::new(
            EngineErrorCode::TransactionLimit,
            format!(
                "a transaction holds at most {} writes and {} bytes of documents; it was rolled back",
                Self::MAX_WRITES,
                Self::MAX_BYTES
            ),
        ))
    }
}
