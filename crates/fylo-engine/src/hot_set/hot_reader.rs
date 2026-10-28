use std::sync::Arc;

use fylo_query::StructuredQuery;
use fylo_storage_native::CollectionKind;

use super::cache::HotSet;
use super::collection_key::CollectionKey;
use super::document_source::DocumentSource;
use super::resident_collection::ResidentCollection;
use super::resident_document::ResidentDocument;
use crate::equality_plan::EqualityPlan;
use crate::{AccessContext, EngineError, ReadDocument, ReadOnlyEngine};

/// Answers reads of one collection from a [`HotSet`], reading storage only
/// for what is not resident.
///
/// Whatever it reads from storage is admitted only if the collection's
/// generation was the resident one before and after the read. Otherwise the
/// hot set was behind the disk: it is cleared and the read is answered from
/// storage, as an engine without a hot set would.
pub(crate) struct HotReader<'a> {
    engine: &'a ReadOnlyEngine,
    hot_set: &'a HotSet,
    key: CollectionKey,
    epoch: u64,
}

/// A resident read that may come back with `None`, meaning the hot set was
/// behind storage, and an inner `None` meaning the actor may not read it.
type ResidentRead = Option<Option<Arc<ResidentDocument>>>;

impl<'a> HotReader<'a> {
    pub(crate) fn new(engine: &'a ReadOnlyEngine, hot_set: &'a HotSet, collection: &str) -> Self {
        Self {
            engine,
            hot_set,
            key: CollectionKey::new(engine.root_path(), collection),
            epoch: hot_set.epoch(),
        }
    }

    pub(crate) fn kind(&self) -> Result<CollectionKind, EngineError> {
        Ok(self.header()?.kind())
    }

    pub(crate) fn get(
        &self,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<ReadDocument, EngineError> {
        let resident = self.resident_get(identifier, actor)?;
        self.answer_get(resident, identifier, actor)
    }

    pub(crate) fn find(
        &self,
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        let resident = self.resident_find(query, actor)?;
        self.answer_find(resident, query, actor)
    }

    pub(crate) const fn engine(&self) -> &ReadOnlyEngine {
        self.engine
    }

    pub(crate) const fn hot_set(&self) -> &HotSet {
        self.hot_set
    }

    pub(crate) const fn key(&self) -> &CollectionKey {
        &self.key
    }

    pub(crate) const fn epoch(&self) -> u64 {
        self.epoch
    }

    fn header(&self) -> Result<Arc<ResidentCollection>, EngineError> {
        match self.hot_set.header(&self.key) {
            Some(header) => Ok(header),
            None => self.load_header(),
        }
    }

    fn load_header(&self) -> Result<Arc<ResidentCollection>, EngineError> {
        let name = self.key.collection();
        let collection = self.engine.open_collection(name)?;
        let header = ReadOnlyEngine::read_stable(&collection, || {
            let generation = ReadOnlyEngine::stable_generation(&collection)?;
            let encrypted = self.engine.encrypted_fields(name)?;
            Ok(ResidentCollection::new(
                collection.kind(),
                generation,
                encrypted,
            ))
        })?;
        let header = Arc::new(header);
        self.hot_set
            .admit_header(self.epoch, &self.key, Arc::clone(&header));
        Ok(header)
    }

    fn resident_get(
        &self,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<ResidentRead, EngineError> {
        let header = self.header()?;
        if !header.is_document_collection() {
            return Ok(None);
        }
        let mut source = DocumentSource::new(self, header);
        let document = source.document(identifier, actor)?;
        Ok(source.finish()?.then_some(document))
    }

    fn answer_get(
        &self,
        resident: ResidentRead,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<ReadDocument, EngineError> {
        let Some(document) = resident else {
            return self
                .engine
                .get_from_storage(self.key.collection(), identifier, actor);
        };
        document
            .map(|document| document.record().clone())
            .ok_or_else(ReadOnlyEngine::access_denied)
    }

    fn resident_find(
        &self,
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
    ) -> Result<Option<Vec<ReadDocument>>, EngineError> {
        let header = self.header()?;
        if !header.is_document_collection() {
            return Ok(None);
        }
        let mut source = DocumentSource::new(self, Arc::clone(&header));
        let candidates = Self::candidates(&mut source, &header, query)?;
        let rows = Self::matching_rows(&mut source, &candidates, query, actor)?;
        Ok(source.finish()?.then_some(rows))
    }

    fn answer_find(
        &self,
        resident: Option<Vec<ReadDocument>>,
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        match resident {
            Some(rows) => Ok(rows),
            None => self
                .engine
                .find_from_storage(self.key.collection(), query, actor),
        }
    }

    /// The index narrows the candidates when the query has a safe equality
    /// plan; otherwise every live document is a candidate.
    fn candidates(
        source: &mut DocumentSource<'_>,
        header: &ResidentCollection,
        query: &StructuredQuery,
    ) -> Result<Arc<Vec<String>>, EngineError> {
        match EqualityPlan::new(query, header.encrypted_fields()) {
            Some(plan) => {
                let snapshot = source.snapshot()?;
                Ok(Arc::new(plan.scan(&snapshot)?))
            }
            None => source.identifiers(),
        }
    }

    /// Candidates in order, stopping at the query's positive limit.
    fn matching_rows(
        source: &mut DocumentSource<'_>,
        candidates: &[String],
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        let mut rows = Vec::new();
        let mut remaining = candidates.iter();
        while let Some(identifier) = Self::next_candidate(&mut remaining, query, rows.len()) {
            rows.extend(Self::matching_row(source, identifier, query, actor)?);
        }
        Ok(rows)
    }

    fn next_candidate<'c>(
        remaining: &mut std::slice::Iter<'c, String>,
        query: &StructuredQuery,
        found: usize,
    ) -> Option<&'c String> {
        if query
            .limit()
            .is_some_and(|limit| limit > 0 && found >= limit)
        {
            return None;
        }
        remaining.next()
    }

    fn matching_row(
        source: &mut DocumentSource<'_>,
        identifier: &str,
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
    ) -> Result<Option<ReadDocument>, EngineError> {
        let document = source.document(identifier, actor)?;
        Ok(document.and_then(|document| Self::matched(&document, query)))
    }

    fn matched(document: &ResidentDocument, query: &StructuredQuery) -> Option<ReadDocument> {
        let record = document.record();
        query
            .matches(
                record.document.fields(),
                record.metadata.created_at,
                record.metadata.updated_at,
            )
            .then(|| record.clone())
    }
}
