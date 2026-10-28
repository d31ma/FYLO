use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use fylo_query::IndexSnapshot;

use super::collection_key::CollectionKey;
use super::collection_slot::CollectionSlot;
use super::hot_set_statistics::HotSetStatistics;
use super::resident_collection::ResidentCollection;
use super::resident_document::ResidentDocument;
use super::resident_entry::ResidentEntry;
use super::resident_part::ResidentPart;

/// The retained entries, their byte total and a clock for eviction.
///
/// Every lookup of a part names the generation it expects, so a part read
/// under one generation is never served for another. Eviction is a
/// second-chance clock: an entry used since the hand last passed it is
/// skipped once. The clock holds exactly one position per resident entry.
pub(crate) struct ResidentStore {
    budget: usize,
    slots: HashMap<Arc<CollectionKey>, CollectionSlot>,
    clock: VecDeque<(Arc<CollectionKey>, ResidentPart)>,
    resident_bytes: usize,
    statistics: HotSetStatistics,
}

impl ResidentStore {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            budget,
            slots: HashMap::new(),
            clock: VecDeque::new(),
            resident_bytes: 0,
            statistics: HotSetStatistics {
                budget_bytes: budget,
                ..HotSetStatistics::default()
            },
        }
    }

    pub(crate) fn header(&mut self, key: &CollectionKey) -> Option<Arc<ResidentCollection>> {
        self.slots.get_mut(key).map(CollectionSlot::header)
    }

    pub(crate) fn snapshot(
        &mut self,
        key: &CollectionKey,
        generation: u64,
    ) -> Option<Arc<IndexSnapshot>> {
        self.slot_at(key, generation)
            .and_then(CollectionSlot::snapshot)
    }

    pub(crate) fn identifiers(
        &mut self,
        key: &CollectionKey,
        generation: u64,
    ) -> Option<Arc<Vec<String>>> {
        self.slot_at(key, generation)
            .and_then(CollectionSlot::identifiers)
    }

    pub(crate) fn document(
        &mut self,
        key: &CollectionKey,
        generation: u64,
        identifier: &str,
    ) -> Option<Arc<ResidentDocument>> {
        let document = self
            .slot_at(key, generation)
            .and_then(|slot| slot.document(identifier));
        self.count_lookup(document.is_some());
        document
    }

    /// Keep a collection header, replacing one read under another generation.
    pub(crate) fn admit_header(
        &mut self,
        key: &CollectionKey,
        header: Arc<ResidentCollection>,
        bytes: usize,
    ) {
        self.drop_other_generation(key, header.generation());
        if self.slots.contains_key(key) || self.refuses(bytes) {
            return;
        }
        self.make_room(bytes);
        let shared = Arc::new(key.clone());
        let slot = CollectionSlot::new(ResidentEntry::new(header, bytes));
        self.slots.insert(Arc::clone(&shared), slot);
        self.account(shared, ResidentPart::Header, bytes);
    }

    pub(crate) fn admit_snapshot(
        &mut self,
        key: &CollectionKey,
        generation: u64,
        snapshot: Arc<IndexSnapshot>,
        bytes: usize,
    ) {
        let entry = ResidentEntry::new(snapshot, bytes);
        if let Some(slot) = self.admit_part(key, generation, ResidentPart::Snapshot, bytes) {
            slot.insert_snapshot(entry);
        }
    }

    pub(crate) fn admit_identifiers(
        &mut self,
        key: &CollectionKey,
        generation: u64,
        identifiers: Arc<Vec<String>>,
        bytes: usize,
    ) {
        let entry = ResidentEntry::new(identifiers, bytes);
        if let Some(slot) = self.admit_part(key, generation, ResidentPart::Identifiers, bytes) {
            slot.insert_identifiers(entry);
        }
    }

    pub(crate) fn admit_document(
        &mut self,
        key: &CollectionKey,
        generation: u64,
        document: Arc<ResidentDocument>,
        bytes: usize,
    ) {
        let identifier = document.record().metadata.id.clone();
        let part = ResidentPart::Document(identifier.clone());
        let entry = ResidentEntry::new(document, bytes);
        if let Some(slot) = self.admit_part(key, generation, part, bytes) {
            slot.insert_document(identifier, entry);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.slots.clear();
        self.clock.clear();
        self.resident_bytes = 0;
        self.statistics.clears += 1;
    }

    pub(crate) fn statistics(&self) -> HotSetStatistics {
        HotSetStatistics {
            resident_bytes: self.resident_bytes,
            collections: self.slots.len(),
            documents: self
                .slots
                .values()
                .map(CollectionSlot::document_count)
                .sum(),
            ..self.statistics
        }
    }

    /// Make room for a part and return the slot to put it in, or `None` when
    /// the part is too large, already resident, or its slot is gone or from
    /// another generation. The caller inserts; the bytes are already counted.
    fn admit_part(
        &mut self,
        key: &CollectionKey,
        generation: u64,
        part: ResidentPart,
        bytes: usize,
    ) -> Option<&mut CollectionSlot> {
        if self.refuses(bytes) {
            return None;
        }
        self.make_room(bytes);
        let shared = self.vacant_slot_key(key, generation, &part)?;
        self.account(Arc::clone(&shared), part, bytes);
        self.slots.get_mut(&*shared)
    }

    fn vacant_slot_key(
        &self,
        key: &CollectionKey,
        generation: u64,
        part: &ResidentPart,
    ) -> Option<Arc<CollectionKey>> {
        self.slots
            .get_key_value(key)
            .filter(|(_, slot)| slot.generation() == generation && !slot.contains(part))
            .map(|(shared, _)| Arc::clone(shared))
    }

    fn slot_at(&mut self, key: &CollectionKey, generation: u64) -> Option<&mut CollectionSlot> {
        self.slots
            .get_mut(key)
            .filter(|slot| slot.generation() == generation)
    }

    fn drop_other_generation(&mut self, key: &CollectionKey, generation: u64) {
        let stale = self
            .slots
            .get(key)
            .is_some_and(|slot| slot.generation() != generation);
        if stale {
            self.remove_slot(key);
        }
    }

    fn refuses(&mut self, bytes: usize) -> bool {
        let refused = bytes > self.budget;
        self.statistics.refusals += u64::from(refused);
        refused
    }

    fn account(&mut self, key: Arc<CollectionKey>, part: ResidentPart, bytes: usize) {
        self.resident_bytes += bytes;
        self.clock.push_back((key, part));
    }

    fn count_lookup(&mut self, found: bool) {
        self.statistics.hits += u64::from(found);
        self.statistics.misses += u64::from(!found);
    }

    fn make_room(&mut self, bytes: usize) {
        while self.resident_bytes + bytes > self.budget && self.evict_one() {}
    }

    /// Advance the clock hand once; false when the clock is empty.
    fn evict_one(&mut self) -> bool {
        let Some((key, part)) = self.clock.pop_front() else {
            return false;
        };
        match self.take_reference(&key, &part) {
            Some(true) => self.clock.push_back((key, part)),
            Some(false) => self.evict(&key, &part),
            None => {}
        }
        true
    }

    fn take_reference(&mut self, key: &CollectionKey, part: &ResidentPart) -> Option<bool> {
        self.slots
            .get_mut(key)
            .and_then(|slot| slot.take_reference(part))
    }

    fn evict(&mut self, key: &CollectionKey, part: &ResidentPart) {
        self.statistics.evictions += 1;
        match part {
            ResidentPart::Header => self.remove_slot(key),
            _ => self.remove_part(key, part),
        }
    }

    /// Drop a whole slot and every clock position that points into it.
    fn remove_slot(&mut self, key: &CollectionKey) {
        let removed = self.slots.remove(key);
        self.resident_bytes -= removed.as_ref().map_or(0, CollectionSlot::bytes);
        self.clock.retain(|(position, _)| position.as_ref() != key);
    }

    fn remove_part(&mut self, key: &CollectionKey, part: &ResidentPart) {
        let freed = self.slots.get_mut(key).map_or(0, |slot| slot.remove(part));
        self.resident_bytes -= freed;
    }
}
