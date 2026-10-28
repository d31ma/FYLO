use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use fylo_query::IndexSnapshot;

use super::collection_key::CollectionKey;
use super::hot_set_statistics::HotSetStatistics;
use super::resident_collection::ResidentCollection;
use super::resident_document::ResidentDocument;
use super::resident_size::ResidentSize;
use super::resident_store::ResidentStore;

/// Parsed documents, index snapshots and collection headers kept in memory
/// under a byte budget, so warm reads make no file calls.
///
/// Every entry is read from storage under one stable collection generation
/// and is served only for that generation. A hot set cannot see a change it
/// was not told about: attach it only where one process is the root's only
/// writer and calls [`Self::clear`] after every frame that may change the
/// root, as `fylo serve` does while it holds the root lease.
///
/// The budget bounds what the hot set retains. A reader still holding an
/// entry after it is evicted keeps that memory alive until it finishes.
pub struct HotSet {
    store: Mutex<ResidentStore>,
    epoch: AtomicU64,
}

impl HotSet {
    const BYTES_PER_KILOBYTE: usize = 1024;
    /// The default budget, the same 64 MiB the shared index cache admits.
    pub const DEFAULT_BUDGET_BYTES: usize =
        64 * Self::BYTES_PER_KILOBYTE * Self::BYTES_PER_KILOBYTE;

    /// An empty hot set that retains at most `budget_bytes`.
    #[must_use]
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            store: Mutex::new(ResidentStore::new(budget_bytes)),
            epoch: AtomicU64::new(0),
        }
    }

    /// Drop every entry. Reads that started before the clear cannot add what
    /// they read afterwards.
    pub fn clear(&self) {
        let mut store = self.lock();
        self.epoch.fetch_add(1, Ordering::SeqCst);
        store.clear();
    }

    /// Changes on every [`Self::clear`].
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Current size, budget and counters.
    #[must_use]
    pub fn statistics(&self) -> HotSetStatistics {
        self.lock().statistics()
    }

    pub(crate) fn header(&self, key: &CollectionKey) -> Option<Arc<ResidentCollection>> {
        self.lock().header(key)
    }

    pub(crate) fn snapshot(
        &self,
        key: &CollectionKey,
        generation: u64,
    ) -> Option<Arc<IndexSnapshot>> {
        self.lock().snapshot(key, generation)
    }

    pub(crate) fn identifiers(
        &self,
        key: &CollectionKey,
        generation: u64,
    ) -> Option<Arc<Vec<String>>> {
        self.lock().identifiers(key, generation)
    }

    pub(crate) fn document(
        &self,
        key: &CollectionKey,
        generation: u64,
        identifier: &str,
    ) -> Option<Arc<ResidentDocument>> {
        self.lock().document(key, generation, identifier)
    }

    pub(crate) fn admit_header(
        &self,
        epoch: u64,
        key: &CollectionKey,
        header: Arc<ResidentCollection>,
    ) {
        let bytes = ResidentSize::header(key, &header);
        if let Some(mut store) = self.lock_at(epoch) {
            store.admit_header(key, header, bytes);
        }
    }

    pub(crate) fn admit_snapshot(
        &self,
        epoch: u64,
        key: &CollectionKey,
        generation: u64,
        snapshot: Arc<IndexSnapshot>,
    ) {
        let bytes = ResidentSize::snapshot(key, &snapshot);
        if let Some(mut store) = self.lock_at(epoch) {
            store.admit_snapshot(key, generation, snapshot, bytes);
        }
    }

    pub(crate) fn admit_identifiers(
        &self,
        epoch: u64,
        key: &CollectionKey,
        generation: u64,
        identifiers: Arc<Vec<String>>,
    ) {
        let bytes = ResidentSize::identifiers(key, &identifiers);
        if let Some(mut store) = self.lock_at(epoch) {
            store.admit_identifiers(key, generation, identifiers, bytes);
        }
    }

    pub(crate) fn admit_document(
        &self,
        epoch: u64,
        key: &CollectionKey,
        generation: u64,
        document: Arc<ResidentDocument>,
    ) {
        let bytes = ResidentSize::document(key, &document.record().metadata.id, document.record());
        if let Some(mut store) = self.lock_at(epoch) {
            store.admit_document(key, generation, document, bytes);
        }
    }

    /// The store, only while no clear has happened since `epoch`.
    fn lock_at(&self, epoch: u64) -> Option<MutexGuard<'_, ResidentStore>> {
        let store = self.lock();
        (self.epoch() == epoch).then_some(store)
    }

    /// A panic while the store was changing may have left its byte count
    /// wrong, so a poisoned store starts empty.
    fn lock(&self) -> MutexGuard<'_, ResidentStore> {
        self.store.lock().unwrap_or_else(|poisoned| {
            self.store.clear_poison();
            self.epoch.fetch_add(1, Ordering::SeqCst);
            let mut store = poisoned.into_inner();
            store.clear();
            store
        })
    }
}
