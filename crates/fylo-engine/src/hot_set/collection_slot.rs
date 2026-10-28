use std::collections::HashMap;
use std::sync::Arc;

use fylo_query::IndexSnapshot;

use super::resident_collection::ResidentCollection;
use super::resident_document::ResidentDocument;
use super::resident_entry::ResidentEntry;
use super::resident_part::ResidentPart;

/// Everything resident for one collection, all read under one generation.
pub(crate) struct CollectionSlot {
    header: ResidentEntry<Arc<ResidentCollection>>,
    snapshot: Option<ResidentEntry<Arc<IndexSnapshot>>>,
    identifiers: Option<ResidentEntry<Arc<Vec<String>>>>,
    documents: HashMap<String, ResidentEntry<Arc<ResidentDocument>>>,
}

impl CollectionSlot {
    pub(crate) fn new(header: ResidentEntry<Arc<ResidentCollection>>) -> Self {
        Self {
            header,
            snapshot: None,
            identifiers: None,
            documents: HashMap::new(),
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.header.value().generation()
    }

    pub(crate) fn header(&mut self) -> Arc<ResidentCollection> {
        self.header.touch()
    }

    pub(crate) fn snapshot(&mut self) -> Option<Arc<IndexSnapshot>> {
        self.snapshot.as_mut().map(ResidentEntry::touch)
    }

    pub(crate) fn identifiers(&mut self) -> Option<Arc<Vec<String>>> {
        self.identifiers.as_mut().map(ResidentEntry::touch)
    }

    pub(crate) fn document(&mut self, identifier: &str) -> Option<Arc<ResidentDocument>> {
        self.documents.get_mut(identifier).map(ResidentEntry::touch)
    }

    pub(crate) fn document_count(&self) -> usize {
        self.documents.len()
    }

    pub(crate) fn contains(&self, part: &ResidentPart) -> bool {
        match part {
            ResidentPart::Header => true,
            ResidentPart::Snapshot => self.snapshot.is_some(),
            ResidentPart::Identifiers => self.identifiers.is_some(),
            ResidentPart::Document(identifier) => self.documents.contains_key(identifier),
        }
    }

    pub(crate) fn insert_snapshot(&mut self, entry: ResidentEntry<Arc<IndexSnapshot>>) {
        self.snapshot = Some(entry);
    }

    pub(crate) fn insert_identifiers(&mut self, entry: ResidentEntry<Arc<Vec<String>>>) {
        self.identifiers = Some(entry);
    }

    pub(crate) fn insert_document(
        &mut self,
        identifier: String,
        entry: ResidentEntry<Arc<ResidentDocument>>,
    ) {
        self.documents.insert(identifier, entry);
    }

    /// Clear one part's reference bit; `None` when the part is gone.
    pub(crate) fn take_reference(&mut self, part: &ResidentPart) -> Option<bool> {
        match part {
            ResidentPart::Header => Some(self.header.take_reference()),
            ResidentPart::Snapshot => self.snapshot.as_mut().map(ResidentEntry::take_reference),
            ResidentPart::Identifiers => {
                self.identifiers.as_mut().map(ResidentEntry::take_reference)
            }
            ResidentPart::Document(identifier) => self
                .documents
                .get_mut(identifier)
                .map(ResidentEntry::take_reference),
        }
    }

    /// Drop one part other than the header and return the bytes it held.
    pub(crate) fn remove(&mut self, part: &ResidentPart) -> usize {
        match part {
            ResidentPart::Header => 0,
            ResidentPart::Snapshot => Self::freed(self.snapshot.take().as_ref()),
            ResidentPart::Identifiers => Self::freed(self.identifiers.take().as_ref()),
            ResidentPart::Document(identifier) => {
                Self::freed(self.documents.remove(identifier).as_ref())
            }
        }
    }

    /// Every byte this slot holds, header included.
    pub(crate) fn bytes(&self) -> usize {
        let mut total = self.header.bytes();
        total += self.snapshot.as_ref().map_or(0, ResidentEntry::bytes);
        total += self.identifiers.as_ref().map_or(0, ResidentEntry::bytes);
        for entry in self.documents.values() {
            total += entry.bytes();
        }
        total
    }

    fn freed<T: Clone>(entry: Option<&ResidentEntry<T>>) -> usize {
        entry.map_or(0, ResidentEntry::bytes)
    }
}
