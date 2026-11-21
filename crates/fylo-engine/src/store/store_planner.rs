use fylo_query::StoreQuery;
use serde_json::{Map, Value};

use super::store_change::StoreChange;
use super::store_overlay::StoreOverlay;
use crate::{EngineError, ReadOnlyEngine};

/// Store writes planned against the committed documents with an open
/// transaction's overlay laid over them (`api/store/v1`, "Documents and
/// ids"). A plan reads only; the caller writes it to storage at once or
/// holds it in its transaction. The machine protocol and the `fylo` crate
/// share these plans, so both answer the same.
impl ReadOnlyEngine {
    /// The fields of the document `id` as the overlay shows it, or `None`
    /// when it does not exist.
    ///
    /// # Errors
    ///
    /// `ENATIVE_NOT_FOUND` (storage) for a missing collection; otherwise a
    /// stable error for unreadable storage.
    pub fn store_record(
        &self,
        collection: &str,
        id: &str,
        overlay: &StoreOverlay,
    ) -> Result<Option<Map<String, Value>>, EngineError> {
        match overlay.state(id) {
            Some(fields) => Ok(fields.cloned()),
            None => self.store_committed_record(collection, id),
        }
    }

    /// `save`: create the document `id` when it does not exist, else
    /// replace its whole body. The change says which.
    ///
    /// # Errors
    ///
    /// As [`Self::store_record`].
    pub fn store_plan_save(
        &self,
        collection: &str,
        (id, fields): (&str, Map<String, Value>),
        overlay: &StoreOverlay,
    ) -> Result<StoreChange, EngineError> {
        let existed = self.store_record(collection, id, overlay)?.is_some();
        Ok(StoreChange::put(id, fields, existed))
    }

    /// `patch`: a shallow merge of top-level `changes` into the document
    /// `id`, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// As [`Self::store_record`].
    pub fn store_plan_patch(
        &self,
        collection: &str,
        (id, changes): (&str, &Map<String, Value>),
        overlay: &StoreOverlay,
    ) -> Result<Option<StoreChange>, EngineError> {
        let current = self.store_record(collection, id, overlay)?;
        Ok(current.map(|fields| Self::store_merged(id, fields, changes)))
    }

    /// `deleteById`: the deletion of the document `id`, or `None` when it
    /// does not exist.
    ///
    /// # Errors
    ///
    /// As [`Self::store_record`].
    pub fn store_plan_delete_by_id(
        &self,
        collection: &str,
        id: &str,
        overlay: &StoreOverlay,
    ) -> Result<Option<StoreChange>, EngineError> {
        let exists = self.store_record(collection, id, overlay)?.is_some();
        Ok(exists.then(|| StoreChange::delete(vec![id.to_owned()])))
    }

    /// `delete(query)`: the deletion of every document the query matches.
    ///
    /// # Errors
    ///
    /// As [`Self::store_matching_ids`].
    pub fn store_plan_delete(
        &self,
        collection: &str,
        query: &StoreQuery,
        overlay: &StoreOverlay,
    ) -> Result<StoreChange, EngineError> {
        let ids = self.store_matching_ids(collection, query, overlay)?;
        Ok(StoreChange::delete(ids))
    }

    fn store_merged(
        id: &str,
        mut fields: Map<String, Value>,
        changes: &Map<String, Value>,
    ) -> StoreChange {
        fields.extend(changes.clone());
        StoreChange::put(id, fields, true)
    }

    fn store_committed_record(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<Option<Map<String, Value>>, EngineError> {
        match self.get(collection, id) {
            Ok(record) => Ok(Some(record.document.fields().clone())),
            Err(error) if self.store_is_missing_document(&error, collection) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// A missing document reads as `None`; a missing collection stays an
    /// error.
    fn store_is_missing_document(&self, error: &EngineError, collection: &str) -> bool {
        let missing =
            error.storage_code() == Some(fylo_storage_native::NativeStorageErrorCode::NotFound);
        missing && self.collection_kind(collection).is_ok()
    }
}
