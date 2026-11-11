use std::collections::BTreeSet;

use fylo_query::{IndexSnapshot, SortOrder, SortPosition, StructuredQuery};
use fylo_storage_native::{NativeCollection, StoredBytes};

use crate::encryption::is_encrypted_field;
use crate::sorted_page::SortedPage;
use crate::{EngineError, EngineErrorCode, ReadDocument, ReadOnlyEngine};

/// Sorted, cursor-paged queries served from the sort field's numeric keys
/// (ADR 0023 sections 5 and 6). Nothing is sorted in memory: the keys are
/// already in order, and only documents that pass the filter's candidates
/// are read.
impl ReadOnlyEngine {
    /// One page of a query with `$sort`, and the cursor of the next page.
    ///
    /// # Errors
    ///
    /// `EENGINE_QUERY` for a query without `$sort` or one that sorts by an
    /// encrypted field; otherwise a stable error for unsafe or corrupt
    /// storage, or a concurrent write.
    pub fn find_sorted(
        &self,
        collection: &str,
        query: &StructuredQuery,
    ) -> Result<SortedPage, EngineError> {
        let sort = Self::require_sort(query)?;
        self.refuse_encrypted_sort(collection, sort)?;
        let collection = self.open_collection(collection)?;
        Self::read_stable(&collection, || self.walk_sorted(&collection, query, sort))
    }

    fn walk_sorted(
        &self,
        collection: &NativeCollection,
        query: &StructuredQuery,
        sort: &SortOrder,
    ) -> Result<SortedPage, EngineError> {
        let candidates = self.sorted_candidates(collection, query)?;
        let snapshot = collection
            .index_snapshot_in_ranges(&[sort.range_after(query.after())])
            .map_err(EngineError::storage)?;
        let positions = Self::sorted_positions(&snapshot, sort, candidates.as_ref());
        let mut page = SortedPage::new(query.limit().filter(|limit| *limit > 0));
        for batch in positions.chunks(Self::read_batch(collection)) {
            let identifiers: Vec<String> = batch.iter().map(|at| at.id().to_owned()).collect();
            let stored = collection
                .read_documents(&identifiers)
                .map_err(EngineError::storage)?;
            for (position, stored) in batch.iter().zip(stored) {
                let record = self.sorted_record(collection, (query, sort), position, stored)?;
                if page.offer(record, position, sort) {
                    return Ok(page);
                }
            }
        }
        Ok(page)
    }

    fn sorted_candidates(
        &self,
        collection: &NativeCollection,
        query: &StructuredQuery,
    ) -> Result<Option<BTreeSet<String>>, EngineError> {
        let planned = self.plan_index_candidates(collection, query)?;
        Ok(planned.map(|identifiers| identifiers.into_iter().collect()))
    }

    fn sorted_positions(
        snapshot: &IndexSnapshot,
        sort: &SortOrder,
        candidates: Option<&BTreeSet<String>>,
    ) -> Vec<SortPosition> {
        snapshot
            .as_bytes()
            .split(|byte| *byte == b'\n')
            .filter_map(|key| sort.position_of_key(key))
            .filter(|position| candidates.is_none_or(|ids| ids.contains(position.id())))
            .collect()
    }

    /// The document at `position` when it matches the query and this key is
    /// the one that places it: a document with several numbers is returned
    /// once, at its first key in the order.
    fn sorted_record(
        &self,
        collection: &NativeCollection,
        (query, sort): (&StructuredQuery, &SortOrder),
        position: &SortPosition,
        stored: StoredBytes,
    ) -> Result<Option<ReadDocument>, EngineError> {
        let record = self.query_record(collection, query, (position.id(), stored))?;
        Ok(record.filter(|record| {
            sort.placing_value(record.document.fields()).as_deref() == Some(position.value())
        }))
    }

    fn require_sort(query: &StructuredQuery) -> Result<&SortOrder, EngineError> {
        query
            .sort()
            .ok_or_else(|| EngineError::new(EngineErrorCode::Query, "a sorted query needs $sort"))
    }

    fn refuse_encrypted_sort(&self, collection: &str, sort: &SortOrder) -> Result<(), EngineError> {
        let encrypted = self.encrypted_fields(collection)?;
        if !is_encrypted_field(sort.field(), &encrypted) {
            return Ok(());
        }
        Err(EngineError::new(
            EngineErrorCode::Query,
            format!(
                "$sort cannot order by the encrypted field \"{}\"",
                sort.field()
            ),
        ))
    }
}
