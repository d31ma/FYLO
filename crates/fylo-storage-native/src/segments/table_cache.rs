use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use super::{LocationTable, SegmentPaths};
use crate::NativeStorageError;

/// Location tables this process has loaded, keyed by collection.
///
/// A cached table is reused while the slot's table epoch is unchanged: the
/// snapshot is the same file, so only location log rows past the length the
/// table already applied are read and laid over it. A rewritten snapshot
/// bumps the epoch and the next caller loads afresh.
pub(crate) struct TableCache {
    tables: Mutex<BTreeMap<PathBuf, (u32, Arc<LocationTable>)>>,
}

impl TableCache {
    pub(crate) fn shared() -> &'static Self {
        static CACHE: OnceLock<TableCache> = OnceLock::new();
        CACHE.get_or_init(|| Self {
            tables: Mutex::new(BTreeMap::new()),
        })
    }

    /// The table covering exactly the first `wal_length` bytes of the log.
    pub(crate) fn table(
        &self,
        paths: &SegmentPaths,
        epoch: u32,
        wal_length: u64,
    ) -> Result<Arc<LocationTable>, NativeStorageError> {
        let key = paths.directory().to_owned();
        let cached = self.cached(&key, epoch);
        let table = match cached {
            Some(table) if table.wal_length() == wal_length => return Ok(table),
            Some(table) if table.wal_length() < wal_length => {
                let mut extended = LocationTable::clone(&table);
                extended.extend(paths, wal_length)?;
                Arc::new(extended)
            }
            Some(_) => return LocationTable::load(paths, wal_length).map(Arc::new),
            None => Arc::new(LocationTable::load(paths, wal_length)?),
        };
        self.lock().insert(key, (epoch, Arc::clone(&table)));
        Ok(table)
    }

    /// Forget the table of one collection, after a rewrite that a slot does
    /// not describe, such as a migration or a recovery.
    pub(crate) fn forget(&self, paths: &SegmentPaths) {
        self.lock().remove(paths.directory());
    }

    fn cached(&self, key: &PathBuf, epoch: u32) -> Option<Arc<LocationTable>> {
        let tables = self.lock();
        let (cached_epoch, table) = tables.get(key)?;
        (*cached_epoch == epoch).then(|| Arc::clone(table))
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<PathBuf, (u32, Arc<LocationTable>)>> {
        self.tables.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
