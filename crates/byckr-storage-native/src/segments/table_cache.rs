use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use super::{LayoutId, LocationTable, MediumIdentity, SegmentMedium, TableReach};
use crate::NativeStorageError;

/// Location tables this process has loaded, keyed by collection.
///
/// A cached table is reused while the slot's layout id and table epoch are
/// unchanged: the snapshot is the same, so only the rows past the reach the
/// table already applied are laid over it — location log rows, or, for a
/// medium without a log, the rows the records after the table's position
/// derive. A rewritten snapshot bumps the epoch and the next caller loads
/// afresh. A layout created again at the same path has another layout id
/// (ADR 0027), so whatever its epoch, it never gets the old layout's table.
pub(crate) struct TableCache {
    tables: Mutex<BTreeMap<MediumIdentity, (TableVersion, Arc<LocationTable>)>>,
}

/// Which layout of a collection, and which of its location snapshots, a
/// cached table was built from.
pub(crate) type TableVersion = (LayoutId, u32);

impl TableCache {
    pub(crate) fn shared() -> &'static Self {
        static CACHE: OnceLock<TableCache> = OnceLock::new();
        CACHE.get_or_init(|| Self {
            tables: Mutex::new(BTreeMap::new()),
        })
    }

    /// The table covering exactly `reach`.
    pub(crate) fn table(
        &self,
        medium: &dyn SegmentMedium,
        version: TableVersion,
        reach: TableReach,
    ) -> Result<Arc<LocationTable>, NativeStorageError> {
        let key = medium.identity();
        let cached = self.cached(&key, version);
        let table = match cached {
            Some(table) if table.reach() == reach => return Ok(table),
            Some(table) if table.reach().precedes(reach) => {
                let mut extended = LocationTable::clone(&table);
                extended.extend(medium, reach)?;
                Arc::new(extended)
            }
            Some(_) => return LocationTable::load(medium, reach).map(Arc::new),
            None => Arc::new(LocationTable::load(medium, reach)?),
        };
        self.lock().insert(key, (version, Arc::clone(&table)));
        Ok(table)
    }

    /// The table covering `reach`, built on the cached table at `cached`
    /// without caching the rows past it. A medium without a location log
    /// gives a writer the records of busy-period members that have not
    /// published yet, and an abort record may still cancel one of them, so
    /// only a published reach is a boundary a cached table may stop at.
    pub(crate) fn table_beyond(
        &self,
        medium: &dyn SegmentMedium,
        version: TableVersion,
        (cached, reach): (TableReach, TableReach),
    ) -> Result<Arc<LocationTable>, NativeStorageError> {
        let table = self.table(medium, version, cached)?;
        if !table.reach().precedes(reach) {
            return Ok(table);
        }
        let mut extended = LocationTable::clone(&table);
        extended.extend(medium, reach)?;
        Ok(Arc::new(extended))
    }

    /// Forget the table of one collection, after a rewrite that a slot does
    /// not describe, such as a migration or a recovery.
    pub(crate) fn forget(&self, medium: &dyn SegmentMedium) {
        self.forget_identity(&medium.identity());
    }

    /// Forget the table of the collection `identity` names.
    pub(crate) fn forget_identity(&self, identity: &MediumIdentity) {
        self.lock().remove(identity);
    }

    fn cached(&self, key: &MediumIdentity, version: TableVersion) -> Option<Arc<LocationTable>> {
        let tables = self.lock();
        let (cached_version, table) = tables.get(key)?;
        (*cached_version == version).then(|| Arc::clone(table))
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<MediumIdentity, (TableVersion, Arc<LocationTable>)>> {
        self.tables.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
