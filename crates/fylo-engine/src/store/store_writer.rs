use fylo_storage_native::{DocumentWrite, NativeStorageErrorCode};

use super::store_change::StoreChange;
use super::store_pending::StorePending;

use crate::{EngineError, WriteEngine};

/// The write half of a Store delete: the documents a Store query selected
/// go in one log record, so recovery removes all of them or none.
impl WriteEngine {
    /// Delete the documents `ids` of one collection in one log record and
    /// return how many were deleted. A root whose profile has no commit log
    /// (the EFS sidecar profile) cannot write one record for many
    /// documents; there they are deleted one at a time, as `delDocs` does.
    ///
    /// # Errors
    ///
    /// A stable error for a missing collection or document, a lease or
    /// identity refusal, or a failed durable write.
    pub fn store_delete_documents(
        &self,
        collection: &str,
        ids: &[String],
    ) -> Result<u64, EngineError> {
        let writes: Vec<DocumentWrite> = ids
            .iter()
            .map(|id| DocumentWrite::Delete {
                identifier: id.clone(),
            })
            .collect();
        let outcome = self.commit_document_writes(collection, &writes);
        self.store_delete_without_log(collection, ids, outcome)?;
        Ok(u64::try_from(ids.len()).unwrap_or(u64::MAX))
    }

    fn store_delete_without_log(
        &self,
        collection: &str,
        ids: &[String],
        outcome: Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        match outcome {
            Err(error) if error.storage_code() == Some(NativeStorageErrorCode::Unsupported) => {
                self.store_delete_each(collection, ids)
            }
            other => other,
        }
    }

    fn store_delete_each(&self, collection: &str, ids: &[String]) -> Result<(), EngineError> {
        for id in ids {
            self.writer
                .delete_document(collection, id)
                .map_err(EngineError::storage)?;
        }
        Ok(())
    }

    /// Write one planned change to storage at once: a put creates or
    /// replaces the document, a deletion goes in one log record.
    ///
    /// # Errors
    ///
    /// As the put, replace and delete it runs.
    pub fn store_apply(&self, collection: &str, change: &StoreChange) -> Result<(), EngineError> {
        match change {
            StoreChange::Put {
                id,
                fields,
                existed: true,
            } => self.patch_document(collection, id, fields.clone()),
            StoreChange::Put {
                id,
                fields,
                existed: false,
            } => self.put_document(collection, id, fields.clone()),
            StoreChange::Delete { ids } => self.store_delete_documents(collection, ids).map(drop),
        }
    }

    /// Commit a transaction's held writes as one log record.
    ///
    /// # Errors
    ///
    /// As [`Self::commit_document_writes`].
    pub fn store_commit(
        &self,
        collection: &str,
        pending: &StorePending,
    ) -> Result<(), EngineError> {
        self.commit_document_writes(collection, &pending.document_writes())
    }
}
