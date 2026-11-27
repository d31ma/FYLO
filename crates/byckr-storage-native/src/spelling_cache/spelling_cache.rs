use super::directory_listing::DirectoryListing;
use super::spelling_state::SpellingState;
use crate::NativeStorageError;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

#[derive(Debug)]
pub(crate) struct SpellingCache {
    state: Mutex<SpellingState>,
}

impl SpellingCache {
    const fn new() -> Self {
        Self {
            state: Mutex::new(SpellingState::new()),
        }
    }

    pub(crate) fn shared() -> &'static Self {
        static CACHE: SpellingCache = SpellingCache::new();
        &CACHE
    }

    pub(crate) fn contains(
        &self,
        directory: &Path,
        name: &OsStr,
    ) -> Result<bool, NativeStorageError> {
        let listing = self.listing_naming(directory, name)?;
        Ok(listing.contains(name))
    }

    pub(crate) fn forget(&self, directory: &Path) {
        self.lock().remove(directory);
    }

    pub(crate) fn observe_generation(&self, scope: &Path, generation: u64) {
        self.lock().observe_generation(scope, generation);
    }

    fn listing_naming(
        &self,
        directory: &Path,
        name: &OsStr,
    ) -> Result<Arc<DirectoryListing>, NativeStorageError> {
        let cached = self.lock().cached(directory);
        match cached {
            Some(listing) if listing.contains(name) => Ok(listing),
            _ => self.refresh(directory),
        }
    }

    fn refresh(&self, directory: &Path) -> Result<Arc<DirectoryListing>, NativeStorageError> {
        let listing = Arc::new(DirectoryListing::read(directory)?);
        self.lock().admit(directory, Arc::clone(&listing));
        Ok(listing)
    }

    fn lock(&self) -> MutexGuard<'_, SpellingState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
