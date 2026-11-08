use std::sync::Arc;

use fylo_query::StructuredQuery;
use fylo_storage_native::{CollectionKind, NativeCollection};

use super::cache::HotSet;
use super::collection_key::CollectionKey;
use super::document_source::DocumentSource;
use super::resident_collection::ResidentCollection;
use super::resident_document::ResidentDocument;
use crate::index_plan::IndexPlan;
use crate::{EngineError, ReadDocument, ReadOnlyEngine};

/// Answers reads of one collection from a [`HotSet`], reading storage only
/// for what is not resident.
///
/// Whatever it reads from storage is admitted only if the collection's
/// generation was the resident one before and after the read. Otherwise the
/// hot set was behind the disk: it is cleared and the read is answered from
/// storage, as an engine without a hot set would. That holds for a read that
/// failed part-way too, such as a listed document deleted before it was read
/// (issue #104): the failure is dropped with the stale state, and storage
/// answers at one stable generation.
pub(crate) struct HotReader<'a> {
    engine: &'a ReadOnlyEngine,
    hot_set: &'a HotSet,
    key: CollectionKey,
    epoch: u64,
}

/// A resident read that may come back with `None`, meaning the hot set was
/// behind storage.
type ResidentRead = Option<Arc<ResidentDocument>>;

/// A header, and the collection it was just read from when it was not
/// resident, so the rest of the read does not open it again.
type HeaderRead = (Arc<ResidentCollection>, Option<NativeCollection>);

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
        Ok(self.header()?.0.kind())
    }

    pub(crate) fn get(&self, identifier: &str) -> Result<ReadDocument, EngineError> {
        let resident = self.resident_get(identifier)?;
        self.answer_get(resident, identifier)
    }

    pub(crate) fn find(&self, query: &StructuredQuery) -> Result<Vec<ReadDocument>, EngineError> {
        let resident = self.resident_find(query)?;
        self.answer_find(resident, query)
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

    fn header(&self) -> Result<HeaderRead, EngineError> {
        match self.hot_set.header(&self.key) {
            Some(header) => Ok((header, None)),
            None => self.load_header(),
        }
    }

    /// One read of `state.json` stamps the header. Whatever is read after it
    /// is checked against that stamp once more before it is admitted, so the
    /// stamp and the reads cannot disagree.
    fn load_header(&self) -> Result<HeaderRead, EngineError> {
        let name = self.key.collection();
        let collection = self.engine.open_collection(name)?;
        let state = ReadOnlyEngine::stable_generation(&collection)?;
        let encrypted = self.engine.encrypted_fields(name)?;
        let guard = match collection.kind() {
            CollectionKind::Document => self.engine.migration_guard(&collection)?,
            CollectionKind::File => None,
        };
        let header = Arc::new(ResidentCollection::new(
            collection.kind(),
            state,
            encrypted,
            guard,
        ));
        self.hot_set
            .admit_header(self.epoch, &self.key, Arc::clone(&header));
        Ok((header, Some(collection)))
    }

    fn resident_get(&self, identifier: &str) -> Result<ResidentRead, EngineError> {
        let (header, collection) = self.header()?;
        if !header.is_document_collection() {
            return Ok(None);
        }
        let mut source = DocumentSource::new(self, header, collection);
        let document = source.document(identifier);
        Self::settled(source.finish()?, document)
    }

    fn answer_get(
        &self,
        resident: ResidentRead,
        identifier: &str,
    ) -> Result<ReadDocument, EngineError> {
        match resident {
            Some(document) => Ok(document.record().clone()),
            None => self
                .engine
                .get_from_storage(self.key.collection(), identifier),
        }
    }

    fn resident_find(
        &self,
        query: &StructuredQuery,
    ) -> Result<Option<Vec<ReadDocument>>, EngineError> {
        let (header, collection) = self.header()?;
        if !header.is_document_collection() {
            return Ok(None);
        }
        let mut source = DocumentSource::new(self, Arc::clone(&header), collection);
        let rows = Self::resident_rows(&mut source, &header, query);
        Self::settled(source.finish()?, rows)
    }

    fn resident_rows(
        source: &mut DocumentSource<'_>,
        header: &ResidentCollection,
        query: &StructuredQuery,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        let candidates = Self::candidates(source, header, query)?;
        Self::matching_rows(source, &candidates, query)
    }

    /// The outcome of a read when storage held still while it ran, or `None`
    /// when the hot set was behind storage. A failed read is kept only in the
    /// first case, so an error caused by the stale state is not reported.
    fn settled<T>(
        consistent: bool,
        read: Result<T, EngineError>,
    ) -> Result<Option<T>, EngineError> {
        if !consistent {
            return Ok(None);
        }
        read.map(Some)
    }

    fn answer_find(
        &self,
        resident: Option<Vec<ReadDocument>>,
        query: &StructuredQuery,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        match resident {
            Some(rows) => Ok(rows),
            None => self.engine.find_from_storage(self.key.collection(), query),
        }
    }

    /// The index narrows the candidates when the query has a safe equality or range
    /// plan, and the index is right for its fields in the head shape;
    /// otherwise every live document is a candidate.
    fn candidates(
        source: &mut DocumentSource<'_>,
        header: &ResidentCollection,
        query: &StructuredQuery,
    ) -> Result<Arc<Vec<String>>, EngineError> {
        let plan = IndexPlan::new(query, header.encrypted_fields()).filter(|plan| {
            header
                .migration_guard()
                .is_none_or(|guard| guard.allows(plan))
        });
        match plan {
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
    ) -> Result<Vec<ReadDocument>, EngineError> {
        let mut rows = Vec::new();
        let mut remaining = candidates.iter();
        while let Some(identifier) = Self::next_candidate(&mut remaining, query, rows.len()) {
            rows.extend(Self::matching_row(source, identifier, query)?);
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
    ) -> Result<Option<ReadDocument>, EngineError> {
        let document = source.document(identifier)?;
        Ok(Self::matched(&document, query))
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
