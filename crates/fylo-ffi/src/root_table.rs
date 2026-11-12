use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use fylo::Root;

use crate::root_entry::RootEntry;

/// The open roots of this process, by handle number. Numbers start at 1 and
/// are never reused, so a stale number finds nothing instead of another root.
#[derive(Debug)]
pub(crate) struct RootTable {
    entries: Mutex<HashMap<u64, Arc<RootEntry>>>,
    next: AtomicU64,
}

impl RootTable {
    pub(crate) fn shared() -> &'static RootTable {
        static TABLE: OnceLock<RootTable> = OnceLock::new();
        TABLE.get_or_init(RootTable::new)
    }

    pub(crate) fn insert(&self, root: Root) -> u64 {
        let handle = self.next.fetch_add(1, Ordering::Relaxed);
        self.lock().insert(handle, Arc::new(RootEntry::new(root)));
        handle
    }

    pub(crate) fn find(&self, handle: u64) -> Option<Arc<RootEntry>> {
        self.lock().get(&handle).cloned()
    }

    pub(crate) fn remove(&self, handle: u64) -> Option<Arc<RootEntry>> {
        self.lock().remove(&handle)
    }

    fn new() -> RootTable {
        RootTable {
            entries: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Arc<RootEntry>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
