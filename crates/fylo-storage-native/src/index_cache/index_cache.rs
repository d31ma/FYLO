use super::IndexInputs;
use super::cache_entry::CacheEntry;
use crate::NativeStorageError;
use fylo_query::IndexSnapshot;
use std::sync::{Arc, Mutex, PoisonError};

pub(crate) struct IndexCache {
    entry: Mutex<Option<CacheEntry>>,
    maximum_retained_bytes: usize,
}

impl IndexCache {
    const BYTES_PER_KILOBYTE: usize = 1024;
    const MAXIMUM_RETAINED_BYTES: usize = 64 * Self::BYTES_PER_KILOBYTE * Self::BYTES_PER_KILOBYTE;

    const fn new(maximum_retained_bytes: usize) -> Self {
        Self {
            entry: Mutex::new(None),
            maximum_retained_bytes,
        }
    }

    pub(crate) fn shared() -> &'static Self {
        static CACHE: IndexCache = IndexCache::new(IndexCache::MAXIMUM_RETAINED_BYTES);
        &CACHE
    }

    pub(crate) fn load(
        &self,
        inputs: IndexInputs,
    ) -> Result<Arc<IndexSnapshot>, NativeStorageError> {
        let cached = self.cached_snapshot(&inputs);
        self.load_cached_or_build(cached, inputs)
    }

    fn cached_snapshot(&self, inputs: &IndexInputs) -> Option<Arc<IndexSnapshot>> {
        let entry = self.entry.lock().unwrap_or_else(PoisonError::into_inner);
        entry.as_ref().and_then(|entry| entry.reuse(inputs))
    }

    fn load_cached_or_build(
        &self,
        cached: Option<Arc<IndexSnapshot>>,
        inputs: IndexInputs,
    ) -> Result<Arc<IndexSnapshot>, NativeStorageError> {
        match cached {
            Some(snapshot) => Ok(snapshot),
            None => self.build_and_admit(inputs),
        }
    }

    fn build_and_admit(
        &self,
        inputs: IndexInputs,
    ) -> Result<Arc<IndexSnapshot>, NativeStorageError> {
        let snapshot = Arc::new(inputs.merge()?);
        let fits_budget = self.fits_budget(&inputs, &snapshot);
        self.admit(inputs, &snapshot, fits_budget);
        Ok(snapshot)
    }

    fn fits_budget(&self, inputs: &IndexInputs, snapshot: &IndexSnapshot) -> bool {
        inputs
            .retained_bytes(snapshot)
            .is_some_and(|bytes| bytes <= self.maximum_retained_bytes)
    }

    fn admit(&self, inputs: IndexInputs, snapshot: &Arc<IndexSnapshot>, fits_budget: bool) {
        if fits_budget {
            self.replace_entry(inputs, snapshot);
        }
    }

    fn replace_entry(&self, inputs: IndexInputs, snapshot: &Arc<IndexSnapshot>) {
        let replacement = CacheEntry::new(inputs, Arc::clone(snapshot));
        let previous = self
            .entry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(replacement);
        drop(previous);
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
