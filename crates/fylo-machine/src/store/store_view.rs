use fylo_engine::{ReadDocument, ReadOnlyEngine, StoreOverlay, StoreScroll};
use fylo_query::{StorePage, StoreQuery, StoreScrollPage};
use serde_json::{Map, Value};

use super::store_failure::StoreFailure;
use crate::MachineError;

/// The documents a Store operation sees: the committed documents, with the
/// writes of its transaction laid over them when it runs inside one.
pub(crate) struct StoreView<'a> {
    engine: ReadOnlyEngine,
    overlay: &'a StoreOverlay,
}

impl<'a> StoreView<'a> {
    pub(crate) const fn new(engine: ReadOnlyEngine, overlay: &'a StoreOverlay) -> Self {
        Self { engine, overlay }
    }

    /// The fields of the document `id`, or `None` when it does not exist. A
    /// missing collection stays `ENATIVE_NOT_FOUND`.
    pub(crate) fn record(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<Option<Map<String, Value>>, MachineError> {
        match self.overlay.state(id) {
            Some(fields) => Ok(fields.cloned()),
            None => self.committed_record(collection, id),
        }
    }

    pub(crate) fn find(
        &self,
        collection: &str,
        query: &StoreQuery,
        page: StorePage,
    ) -> Result<Vec<ReadDocument>, MachineError> {
        self.engine
            .store_find(collection, query, page, self.overlay)
            .map_err(|error| StoreFailure::engine(&error))
    }

    pub(crate) fn scroll(
        &self,
        collection: &str,
        query: &StoreQuery,
        page: &StoreScrollPage,
    ) -> Result<StoreScroll, MachineError> {
        self.engine
            .store_scroll(collection, query, page, self.overlay)
            .map_err(|error| StoreFailure::engine(&error))
    }

    pub(crate) fn count(&self, collection: &str, query: &StoreQuery) -> Result<u64, MachineError> {
        self.engine
            .store_count(collection, query, self.overlay)
            .map_err(|error| StoreFailure::engine(&error))
    }

    pub(crate) fn exists(
        &self,
        collection: &str,
        query: &StoreQuery,
    ) -> Result<bool, MachineError> {
        self.engine
            .store_exists(collection, query, self.overlay)
            .map_err(|error| StoreFailure::engine(&error))
    }

    pub(crate) fn matching_ids(
        &self,
        collection: &str,
        query: &StoreQuery,
    ) -> Result<Vec<String>, MachineError> {
        self.engine
            .store_matching_ids(collection, query, self.overlay)
            .map_err(|error| StoreFailure::engine(&error))
    }

    fn committed_record(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<Option<Map<String, Value>>, MachineError> {
        match self.engine.get(collection, id) {
            Ok(record) => Ok(Some(record.document.fields().clone())),
            Err(error) if self.is_missing_document(&error, collection) => Ok(None),
            Err(error) => Err(StoreFailure::engine(&error)),
        }
    }

    fn is_missing_document(&self, error: &fylo_engine::EngineError, collection: &str) -> bool {
        let collection_present = self.engine.collection_kind(collection).is_ok();
        StoreFailure::is_missing(error) && collection_present
    }
}
