use byckr_engine::{
    EngineError, ReadDocument, ReadOnlyEngine, StoreChange, StoreOverlay, StoreScroll,
};
use byckr_query::{StorePage, StoreQuery, StoreScrollPage};
use serde_json::{Map, Value};

use super::store_failure::StoreFailure;
use crate::MachineError;

/// The documents a Store operation sees: the committed documents, with the
/// writes of its transaction laid over them when it runs inside one. Every
/// read and plan goes through the engine's Store plan.
pub(crate) struct StoreView<'a> {
    engine: ReadOnlyEngine,
    overlay: &'a StoreOverlay,
}

impl<'a> StoreView<'a> {
    pub(crate) const fn new(engine: ReadOnlyEngine, overlay: &'a StoreOverlay) -> Self {
        Self { engine, overlay }
    }

    pub(crate) fn record(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<Option<Map<String, Value>>, MachineError> {
        Self::checked(self.engine.store_record(collection, id, self.overlay))
    }

    pub(crate) fn find(
        &self,
        collection: &str,
        query: &StoreQuery,
        page: StorePage,
    ) -> Result<Vec<ReadDocument>, MachineError> {
        Self::checked(
            self.engine
                .store_find(collection, query, page, self.overlay),
        )
    }

    pub(crate) fn scroll(
        &self,
        collection: &str,
        query: &StoreQuery,
        page: &StoreScrollPage,
    ) -> Result<StoreScroll, MachineError> {
        Self::checked(
            self.engine
                .store_scroll(collection, query, page, self.overlay),
        )
    }

    pub(crate) fn count(&self, collection: &str, query: &StoreQuery) -> Result<u64, MachineError> {
        Self::checked(self.engine.store_count(collection, query, self.overlay))
    }

    pub(crate) fn exists(
        &self,
        collection: &str,
        query: &StoreQuery,
    ) -> Result<bool, MachineError> {
        Self::checked(self.engine.store_exists(collection, query, self.overlay))
    }

    pub(crate) fn plan_save(
        &self,
        collection: &str,
        entity: (&str, Map<String, Value>),
    ) -> Result<StoreChange, MachineError> {
        Self::checked(
            self.engine
                .store_plan_save(collection, entity, self.overlay),
        )
    }

    pub(crate) fn plan_patch(
        &self,
        collection: &str,
        changes: (&str, &Map<String, Value>),
    ) -> Result<Option<StoreChange>, MachineError> {
        Self::checked(
            self.engine
                .store_plan_patch(collection, changes, self.overlay),
        )
    }

    pub(crate) fn plan_delete_by_id(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<Option<StoreChange>, MachineError> {
        Self::checked(
            self.engine
                .store_plan_delete_by_id(collection, id, self.overlay),
        )
    }

    pub(crate) fn plan_delete(
        &self,
        collection: &str,
        query: &StoreQuery,
    ) -> Result<StoreChange, MachineError> {
        Self::checked(
            self.engine
                .store_plan_delete(collection, query, self.overlay),
        )
    }

    fn checked<T>(outcome: Result<T, EngineError>) -> Result<T, MachineError> {
        outcome.map_err(|error| StoreFailure::engine(&error))
    }
}
