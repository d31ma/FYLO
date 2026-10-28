use std::sync::Arc;

use fylo_query::IndexSnapshot;
use fylo_storage_native::NativeCollection;

use super::hot_reader::HotReader;
use super::resident_collection::ResidentCollection;
use super::resident_document::ResidentDocument;
use crate::{AccessContext, EngineError, ReadOnlyEngine};

/// The parts one read needs, from the hot set when resident and from storage
/// otherwise.
///
/// The collection is opened on the first miss, and its generation and log end
/// are checked then and again in [`Self::finish`]. What storage gave is
/// admitted only if both checks saw the resident ones, still stable.
pub(crate) struct DocumentSource<'a> {
    reader: &'a HotReader<'a>,
    header: Arc<ResidentCollection>,
    collection: Option<NativeCollection>,
    consistent: bool,
    documents: Vec<Arc<ResidentDocument>>,
    snapshot: Option<Arc<IndexSnapshot>>,
    identifiers: Option<Arc<Vec<String>>>,
}

impl<'a> DocumentSource<'a> {
    pub(crate) const fn new(reader: &'a HotReader<'a>, header: Arc<ResidentCollection>) -> Self {
        Self {
            reader,
            header,
            collection: None,
            consistent: true,
            documents: Vec::new(),
            snapshot: None,
            identifiers: None,
        }
    }

    /// One document the actor may read, or `None` when its descriptor denies
    /// the actor. A denied document is not parsed, as without a hot set.
    pub(crate) fn document(
        &mut self,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<Option<Arc<ResidentDocument>>, EngineError> {
        let document = self.resident_or_stored(identifier, actor)?;
        Ok(document.filter(|document| crate::read_access_allowed(document.access(), actor)))
    }

    pub(crate) fn snapshot(&mut self) -> Result<Arc<IndexSnapshot>, EngineError> {
        let resident = self
            .reader
            .hot_set()
            .snapshot(self.reader.key(), self.generation());
        match resident {
            Some(snapshot) => Ok(snapshot),
            None => self.stored_snapshot(),
        }
    }

    pub(crate) fn identifiers(&mut self) -> Result<Arc<Vec<String>>, EngineError> {
        let resident = self
            .reader
            .hot_set()
            .identifiers(self.reader.key(), self.generation());
        match resident {
            Some(identifiers) => Ok(identifiers),
            None => self.stored_identifiers(),
        }
    }

    /// True when everything read is consistent with the resident generation;
    /// then it is admitted. False clears the hot set.
    pub(crate) fn finish(mut self) -> Result<bool, EngineError> {
        let Some(collection) = self.collection.take() else {
            return Ok(true);
        };
        self.observe_generation(&collection)?;
        Ok(self.settle())
    }

    fn generation(&self) -> u64 {
        self.header.generation()
    }

    fn resident_or_stored(
        &mut self,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<Option<Arc<ResidentDocument>>, EngineError> {
        let resident =
            self.reader
                .hot_set()
                .document(self.reader.key(), self.generation(), identifier);
        match resident {
            Some(document) => Ok(Some(document)),
            None => self.stored_document(identifier, actor),
        }
    }

    fn stored_document(
        &mut self,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<Option<Arc<ResidentDocument>>, EngineError> {
        let stored = self
            .collection()?
            .read_document(identifier)
            .map_err(EngineError::storage)?;
        if !crate::read_access_allowed(stored.access, actor) {
            return Ok(None);
        }
        let record = self.reader.engine().build_record(
            self.reader.key().collection(),
            identifier,
            &stored,
        )?;
        let document = Arc::new(ResidentDocument::new(record, stored.access));
        self.documents.push(Arc::clone(&document));
        Ok(Some(document))
    }

    fn stored_snapshot(&mut self) -> Result<Arc<IndexSnapshot>, EngineError> {
        let snapshot = self
            .collection()?
            .shared_index_snapshot()
            .map_err(EngineError::storage)?;
        self.snapshot = Some(Arc::clone(&snapshot));
        Ok(snapshot)
    }

    fn stored_identifiers(&mut self) -> Result<Arc<Vec<String>>, EngineError> {
        let identifiers = self
            .collection()?
            .document_ids()
            .map_err(EngineError::storage)?;
        let identifiers = Arc::new(identifiers);
        self.identifiers = Some(Arc::clone(&identifiers));
        Ok(identifiers)
    }

    /// The open collection, opened and checked on first use.
    fn collection(&mut self) -> Result<&NativeCollection, EngineError> {
        let collection = match self.collection.take() {
            Some(collection) => collection,
            None => self.open()?,
        };
        Ok(self.collection.insert(collection))
    }

    fn open(&mut self) -> Result<NativeCollection, EngineError> {
        let collection = self
            .reader
            .engine()
            .open_collection(self.reader.key().collection())?;
        self.observe_generation(&collection)?;
        Ok(collection)
    }

    fn observe_generation(&mut self, collection: &NativeCollection) -> Result<(), EngineError> {
        let state = ReadOnlyEngine::read_attempt_generation(collection)?;
        self.consistent &= state.is_some_and(|state| self.header.read_at(&state));
        Ok(())
    }

    fn settle(self) -> bool {
        if !self.consistent {
            self.reader.hot_set().clear();
            return false;
        }
        self.admit();
        true
    }

    fn admit(self) {
        let hot_set = self.reader.hot_set();
        let (epoch, key, generation) = (self.reader.epoch(), self.reader.key(), self.generation());
        for document in self.documents {
            hot_set.admit_document(epoch, key, generation, document);
        }
        if let Some(snapshot) = self.snapshot {
            hot_set.admit_snapshot(epoch, key, generation, snapshot);
        }
        if let Some(identifiers) = self.identifiers {
            hot_set.admit_identifiers(epoch, key, generation, identifiers);
        }
    }
}
