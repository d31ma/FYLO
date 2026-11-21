use fylo_format::{CanonicalMetadata, Document, DocumentLimits, TtidTimestamps};
use fylo_query::{StoreNarrowing, StorePage, StorePosition, StoreQuery, StoreRow, StoreScrollPage};
use fylo_storage_native::{NativeCollection, NativeStorageErrorCode, StoredBytes};
use serde_json::{Map, Value};

use super::store_counter::StoreCounter;
use super::store_existence::StoreExistence;
use super::store_id_list::StoreIdList;
use super::store_overlay::StoreOverlay;
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
        overlay: &StoreOverlay,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        let handle = self.open_collection(collection)?;
        Self::read_stable(&handle, || {
            let positions = self.store_ranked(&handle, (query, overlay), page.end(), None)?;
            let page_positions: Vec<StorePosition> =
                positions.into_iter().skip(page.offset()).collect();
            self.store_read_positions(&handle, &page_positions, overlay)
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
        overlay: &StoreOverlay,
    ) -> Result<StoreScroll, EngineError> {
        let handle = self.open_collection(collection)?;
        let after = Self::store_cursor_position(&handle, query, page)?;
        Self::read_stable(&handle, || {
            let positions = self.store_ranked(
                &handle,
                (query, overlay),
                page.size().saturating_add(1),
                after.clone(),
            )?;
            let next_cursor = Self::store_next_cursor(&handle, query, &positions, page.size());
            let shown: Vec<StorePosition> = positions.into_iter().take(page.size()).collect();
            let content = self.store_read_positions(&handle, &shown, overlay)?;
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
    pub fn store_count(
        &self,
        collection: &str,
        query: &StoreQuery,
        overlay: &StoreOverlay,
    ) -> Result<u64, EngineError> {
        let handle = self.open_collection(collection)?;
        Self::read_stable(&handle, || {
            let mut counter = StoreCounter::default();
            self.store_scan(&handle, (query, overlay), &mut counter)?;
            Ok(counter.count())
        })
    }

    /// Whether a Store query matches any document; `false` for a missing
    /// collection.
    ///
    /// # Errors
    ///
    /// A stable error for unreadable storage or a concurrent write.
    pub fn store_exists(
        &self,
        collection: &str,
        query: &StoreQuery,
        overlay: &StoreOverlay,
    ) -> Result<bool, EngineError> {
        let handle = self.open_collection_if_present(collection)?;
        handle.map_or(Ok(false), |handle| {
            self.store_exists_in(&handle, (query, overlay))
        })
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
        overlay: &StoreOverlay,
    ) -> Result<Vec<String>, EngineError> {
        let handle = self.open_collection(collection)?;
        Self::read_stable(&handle, || {
            let mut ids = StoreIdList::default();
            self.store_scan(&handle, (query, overlay), &mut ids)?;
            let mut ids = ids.into_ids();
            ids.sort();
            Ok(ids)
        })
    }

    fn store_exists_in(
        &self,
        handle: &NativeCollection,
        view: (&StoreQuery, &StoreOverlay),
    ) -> Result<bool, EngineError> {
        Self::read_stable(handle, || {
            let mut existence = StoreExistence::default();
            self.store_scan(handle, view, &mut existence)?;
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
        view: (&StoreQuery, &StoreOverlay),
        limit: usize,
        after: Option<StorePosition>,
    ) -> Result<Vec<StorePosition>, EngineError> {
        let mut ranking = StoreRanking::new(limit, after);
        self.store_scan(handle, view, &mut ranking)?;
        Ok(ranking.into_sorted())
    }

    fn store_read_positions(
        &self,
        handle: &NativeCollection,
        positions: &[StorePosition],
        overlay: &StoreOverlay,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        let stored_ids: Vec<String> = positions
            .iter()
            .map(|position| position.id().to_owned())
            .filter(|id| !overlay.touches(id))
            .collect();
        let stored = handle
            .read_documents(&stored_ids)
            .map_err(EngineError::storage)?;
        let mut from_storage = stored_ids.iter().zip(stored);
        positions
            .iter()
            .map(|position| {
                self.store_page_record(handle, (position.id(), overlay), &mut from_storage)
            })
            .collect()
    }

    fn store_page_record<'a>(
        &self,
        handle: &NativeCollection,
        (id, overlay): (&str, &StoreOverlay),
        from_storage: &mut impl Iterator<Item = (&'a String, StoredBytes)>,
    ) -> Result<ReadDocument, EngineError> {
        if let Some(Some(fields)) = overlay.state(id) {
            return Self::store_overlay_record(id, fields);
        }
        let (stored_id, stored) = from_storage.next().ok_or_else(|| {
            EngineError::new(
                crate::EngineErrorCode::CorruptData,
                "a Store page lost a document",
            )
        })?;
        self.build_record(handle.name(), stored_id, &stored)
    }

    /// The record of a document an open transaction wrote: its fields and
    /// the times its TTID holds.
    fn store_overlay_record(
        id: &str,
        fields: &Map<String, Value>,
    ) -> Result<ReadDocument, EngineError> {
        let document =
            Document::try_from_value(Value::Object(fields.clone()), DocumentLimits::default())
                .map_err(EngineError::format)?;
        let timestamps = TtidTimestamps::decode(id).map_err(EngineError::format)?;
        Ok(ReadDocument {
            metadata: CanonicalMetadata {
                id: id.to_owned(),
                created_at: timestamps.created_at,
                updated_at: timestamps.created_at,
                mtime: timestamps.created_at,
            },
            document,
        })
    }

    /// Offer every document the query matches to `sink`: the stored
    /// documents the transaction's overlay does not touch, then the
    /// documents the overlay holds.
    fn store_scan(
        &self,
        handle: &NativeCollection,
        (query, overlay): (&StoreQuery, &StoreOverlay),
        sink: &mut dyn StoreSink,
    ) -> Result<(), EngineError> {
        let mut ids = self.store_candidates(handle, query)?;
        ids.retain(|id| !overlay.touches(id));
        for batch in ids.chunks(Self::read_batch(handle)) {
            let stored = handle.read_documents(batch).map_err(EngineError::storage)?;
            if self.store_offer_batch(handle, query, (batch, stored), sink)? {
                return Ok(());
            }
        }
        Self::store_offer_overlay(query, overlay, sink)
    }

    fn store_offer_overlay(
        query: &StoreQuery,
        overlay: &StoreOverlay,
        sink: &mut dyn StoreSink,
    ) -> Result<(), EngineError> {
        for (id, fields) in overlay.present() {
            let record = Self::store_overlay_record(id, fields)?;
            if Self::store_offer(query, &record, sink)? {
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
