use super::directory_listing::DirectoryListing;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct SpellingState {
    listings: BTreeMap<PathBuf, Arc<DirectoryListing>>,
    generations: BTreeMap<PathBuf, u64>,
    retained_names: usize,
}

impl SpellingState {
    const MAXIMUM_RETAINED_NAMES: usize = 1 << 20;

    pub(super) const fn new() -> Self {
        Self {
            listings: BTreeMap::new(),
            generations: BTreeMap::new(),
            retained_names: 0,
        }
    }

    pub(super) fn cached(&self, directory: &Path) -> Option<Arc<DirectoryListing>> {
        self.listings.get(directory).cloned()
    }

    pub(super) fn admit(&mut self, directory: &Path, listing: Arc<DirectoryListing>) {
        self.remove(directory);
        self.make_room(listing.len());
        self.retained_names += listing.len();
        self.listings.insert(directory.to_path_buf(), listing);
    }

    pub(super) fn remove(&mut self, directory: &Path) {
        let removed = self.listings.remove(directory);
        self.retained_names -= removed.map_or(0, |listing| listing.len());
    }

    pub(super) fn observe_generation(&mut self, scope: &Path, generation: u64) {
        let previous = self.generations.insert(scope.to_path_buf(), generation);
        self.remove_within_if_changed(scope, previous != Some(generation));
    }

    fn remove_within_if_changed(&mut self, scope: &Path, changed: bool) {
        if changed {
            self.listings
                .retain(|directory, _| !directory.starts_with(scope));
            self.retained_names = self.listings.values().map(|listing| listing.len()).sum();
        }
    }

    fn make_room(&mut self, incoming_names: usize) {
        if self.retained_names + incoming_names > Self::MAXIMUM_RETAINED_NAMES {
            self.listings.clear();
            self.retained_names = 0;
        }
    }
}
