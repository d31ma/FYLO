use fylo_query::{StoreNarrowing, StorePage, StorePosition, StoreQuery, StoreRow, StoreScrollPage};
use fylo_storage_native::{NativeCollection, NativeStorageErrorCode, StoredBytes};

use super::store_counter::StoreCounter;
use super::store_existence::StoreExistence;
use super::store_id_list::StoreIdList;
use super::store_ranking::StoreRanking;
use super::store_scroll::StoreScroll;
use super::store_sink::StoreSink;
use crate::{EngineError, ReadDocument, ReadOnlyEngine};

/// The Store's query plan (`api/store/v1`): one scan for find, scroll,
/// count, exists and the selection of a delete. The prefix index narrows
/// which documents are read when the query allows it; each document read
/// is decided by the Store's strict rules; sorting and paging keep only
/// positions, so no page has a bound on its offset.
impl ReadOnlyEngine {
    /// One offset page of the documents a Store query matches, in its order.
    ///
    /// # Errors
    ///
    /// `ENATIVE_NOT_FOUND` (storage) for a missing collection; a query
    /// error for a sort field that holds an object or an array; otherwise a
    /// stable error for unreadable storage or a concurrent write.
    pub fn store_find(
        &self,
        collection: &str,
        query: &StoreQuery,
        page: StorePage,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        let handle = self.open_collection(collection)?;
        Self::read_stable(&handle, || {
            let positions = self.store_ranked(&handle, query, page.end(), None)?;
            let page_positions: Vec<StorePosition> =
                positions.into_iter().skip(page.offset()).collect();
            self.store_read_positions(&handle, &page_positions)
        })
    }

    /// One cursor page of the documents a Store query matches, and the
    /// cursor of the next page.
    ///
    /// # Errors
    ///
    /// As [`Self::store_find`], plus `ECURSOR` (query) for a cursor of
    /// another query or collection.
    pub fn store_scroll(
        &self,
        collection: &str,
        query: &StoreQuery,
        page: &StoreScrollPage,
    ) -> Result<StoreScroll, EngineError> {
        let handle = self.open_collection(collection)?;
        let after = Self::store_cursor_position(&handle, query, page)?;
        Self::read_stable(&handle, || {
            let positions =
                self.store_ranked(&handle, query, page.size().saturating_add(1), after.clone())?;
            let next_cursor = Self::store_next_cursor(&handle, query, &positions, page.size());
            let shown: Vec<StorePosition> = positions.into_iter().take(page.size()).collect();
            let content = self.store_read_positions(&handle, &shown)?;
            Ok(StoreScroll {
                content,
                next_cursor,
            })
        })
    }

    /// How many documents a Store query matches, exact at the moment of
    /// the call.
    ///
    /// # Errors
    ///
    /// As [`Self::store_find`].
    pub fn store_count(&self, collection: &str, query: &StoreQuery) -> Result<u64, EngineError> {
        let handle = self.open_collection(collection)?;
        Self::read_stable(&handle, || {
            let mut counter = StoreCounter::default();
            self.store_scan(&handle, query, &mut counter)?;
            Ok(counter.count())
        })
    }

    /// Whether a Store query matches any document; `false` for a missing
    /// collection.
    ///
    /// # Errors
    ///
    /// A stable error for unreadable storage or a concurrent write.
    pub fn store_exists(&self, collection: &str, query: &StoreQuery) -> Result<bool, EngineError> {
        let handle = self.open_collection_if_present(collection)?;
        handle.map_or(Ok(false), |handle| self.store_exists_in(&handle, query))
    }

    /// The id of every document a Store query matches, in id order: the
    /// selection of a Store delete.
    ///
    /// # Errors
    ///
    /// As [`Self::store_find`].
    pub fn store_matching_ids(
        &self,
        collection: &str,
        query: &StoreQuery,
    ) -> Result<Vec<String>, EngineError> {
        let handle = self.open_collection(collection)?;
        Self::read_stable(&handle, || {
            let mut ids = StoreIdList::default();
            self.store_scan(&handle, query, &mut ids)?;
            let mut ids = ids.into_ids();
            ids.sort();
            Ok(ids)
        })
    }

    fn store_exists_in(
        &self,
        handle: &NativeCollection,
        query: &StoreQuery,
    ) -> Result<bool, EngineError> {
        Self::read_stable(handle, || {
            let mut existence = StoreExistence::default();
            self.store_scan(handle, query, &mut existence)?;
            Ok(existence.found())
        })
    }

    fn open_collection_if_present(
        &self,
        collection: &str,
    ) -> Result<Option<NativeCollection>, EngineError> {
        match self.open_collection(collection) {
            Ok(handle) => Ok(Some(handle)),
            Err(error) if error.storage_code() == Some(NativeStorageErrorCode::NotFound) => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn store_cursor_position(
        handle: &NativeCollection,
        query: &StoreQuery,
        page: &StoreScrollPage,
    ) -> Result<Option<StorePosition>, EngineError> {
        page.cursor()
            .map(|cursor| query.cursor_position(cursor, handle.name()))
            .transpose()
            .map_err(EngineError::query)
    }

    fn store_next_cursor(
        handle: &NativeCollection,
        query: &StoreQuery,
        positions: &[StorePosition],
        size: usize,
    ) -> Option<String> {
        if positions.len() <= size {
            return None;
        }
        positions
            .get(size.checked_sub(1)?)
            .map(|last| query.cursor(last, handle.name()))
    }

    fn store_ranked(
        &self,
        handle: &NativeCollection,
        query: &StoreQuery,
        limit: usize,
        after: Option<StorePosition>,
    ) -> Result<Vec<StorePosition>, EngineError> {
        let mut ranking = StoreRanking::new(limit, after);
        self.store_scan(handle, query, &mut ranking)?;
        Ok(ranking.into_sorted())
    }

    fn store_read_positions(
        &self,
        handle: &NativeCollection,
        positions: &[StorePosition],
    ) -> Result<Vec<ReadDocument>, EngineError> {
        let ids: Vec<String> = positions
            .iter()
            .map(|position| position.id().to_owned())
            .collect();
        let stored = handle.read_documents(&ids).map_err(EngineError::storage)?;
        ids.iter()
            .zip(stored)
            .map(|(id, stored)| self.build_record(handle.name(), id, &stored))
            .collect()
    }

    fn store_scan(
        &self,
        handle: &NativeCollection,
        query: &StoreQuery,
        sink: &mut dyn StoreSink,
    ) -> Result<(), EngineError> {
        let ids = self.store_candidates(handle, query)?;
        for batch in ids.chunks(Self::read_batch(handle)) {
            let stored = handle.read_documents(batch).map_err(EngineError::storage)?;
            if self.store_offer_batch(handle, query, (batch, stored), sink)? {
                return Ok(());
            }
        }
        Ok(())
    }

    fn store_offer_batch(
        &self,
        handle: &NativeCollection,
        query: &StoreQuery,
        (ids, stored): (&[String], Vec<StoredBytes>),
        sink: &mut dyn StoreSink,
    ) -> Result<bool, EngineError> {
        for (id, stored) in ids.iter().zip(stored) {
            let record = self.build_record(handle.name(), id, &stored)?;
            if Self::store_offer(query, &record, sink)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn store_offer(
        query: &StoreQuery,
        record: &ReadDocument,
        sink: &mut dyn StoreSink,
    ) -> Result<bool, EngineError> {
        let row = StoreRow::new(&record.metadata.id, record.document.fields());
        if !query.matches(&row) {
            return Ok(false);
        }
        sink.offer(query, &row)
    }

    fn store_candidates(
        &self,
        handle: &NativeCollection,
        query: &StoreQuery,
    ) -> Result<Vec<String>, EngineError> {
        let planned = self.store_planned_candidates(handle, query)?;
        planned.map_or_else(|| handle.document_ids().map_err(EngineError::storage), Ok)
    }

    fn store_planned_candidates(
        &self,
        handle: &NativeCollection,
        query: &StoreQuery,
    ) -> Result<Option<Vec<String>>, EngineError> {
        let Some(narrowing) = StoreNarrowing::structured(query) else {
            return Ok(None);
        };
        self.plan_index_candidates(handle, &narrowing)
    }
}
