use std::path::{Path, PathBuf};

/// One collection of one root: the scope of every resident entry.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CollectionKey {
    root: PathBuf,
    collection: String,
}

impl CollectionKey {
    pub(crate) fn new(root: &Path, collection: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            collection: collection.to_owned(),
        }
    }

    pub(crate) fn collection(&self) -> &str {
        &self.collection
    }

    /// Heap bytes this key holds, for the budget.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.root.as_os_str().len() + self.collection.capacity()
    }
}
