use std::io::Read;

use crate::index_cache::IndexInputs;
use crate::index_limit::IndexLimit;
use crate::temp_tier::{EntryKey, IndexEntry, PendingEntry, TempCacheTier};
use crate::{NativeCollection, NativeStorageError};

/// How a files-layout collection reads its index bytes through the temp
/// tier (ADR 0030 slice S5).
///
/// The index snapshot and its write-ahead log are kept as read from the
/// root, under the stamp of the read that read them, in one entry per
/// collection. They are used only while the collection has that same stamp,
/// and still go through the content-verified index cache. An index read from
/// the root becomes an entry only after the next generation read sees the
/// same stamp, as a document does.
impl NativeCollection {
    /// The index inputs, from the temp tier when it holds them for the
    /// current stamp, else from the root.
    pub(crate) fn index_inputs(&self, log_limit: u64) -> Result<IndexInputs, NativeStorageError> {
        let cached = self.cached_index();
        cached.map_or_else(|| self.read_and_keep_index(log_limit), Ok)
    }

    /// The index inputs for a range read: from the temp tier, or read whole
    /// from the root and kept when the snapshot is small enough to keep;
    /// `None` when the range read should read the root's file in place.
    pub(crate) fn index_inputs_for_ranges(
        &self,
    ) -> Result<Option<IndexInputs>, NativeStorageError> {
        let Some((tier, stamp)) = self.index_tier() else {
            return Ok(None);
        };
        let cached = self.cached_index();
        cached.map_or_else(
            || self.read_small_index(tier, stamp),
            |inputs| Ok(Some(inputs)),
        )
    }

    fn index_tier(&self) -> Option<(&TempCacheTier, [u8; 32])> {
        let (tier, _) = self.tier()?;
        Some((tier, self.window.stamp()?))
    }

    fn index_key(&self) -> Option<EntryKey> {
        let (_, root_key) = self.tier()?;
        Some(EntryKey::index(root_key, &self.cache_name()))
    }

    fn cached_index(&self) -> Option<IndexInputs> {
        let (tier, stamp) = self.index_tier()?;
        let key = self.index_key()?;
        let entry = tier.read_index(&key, &self.cache_name());
        let current = entry.filter(|entry| entry.collection_stamp == stamp);
        Self::count_index(tier, current.is_some());
        current.map(|entry| IndexInputs {
            snapshot: entry.snapshot,
            log: entry.log,
        })
    }

    fn count_index(tier: &TempCacheTier, hit: bool) {
        if hit {
            tier.count_hit();
            return;
        }
        tier.count_miss();
    }

    fn read_and_keep_index(&self, log_limit: u64) -> Result<IndexInputs, NativeStorageError> {
        let inputs = IndexInputs {
            snapshot: self
                .root
                .read_file(&self.index_snapshot_path(), IndexLimit::INDEX_BYTES)?,
            log: self.index_log(log_limit)?,
        };
        self.keep_index(&inputs);
        Ok(inputs)
    }

    fn read_small_index(
        &self,
        tier: &TempCacheTier,
        stamp: [u8; 32],
    ) -> Result<Option<IndexInputs>, NativeStorageError> {
        let (mut file, metadata) = self
            .root
            .open_file(&self.index_snapshot_path(), IndexLimit::INDEX_BYTES)?;
        let small = usize::try_from(metadata.len()).is_ok_and(|bytes| tier.admits(bytes));
        if !small {
            return Ok(None);
        }
        let mut snapshot = Vec::new();
        (&mut file)
            .take(IndexLimit::INDEX_BYTES.saturating_add(1))
            .read_to_end(&mut snapshot)
            .map_err(NativeStorageError::io)?;
        let inputs = IndexInputs {
            snapshot,
            log: self.index_log(crate::MAX_INDEX_WAL_BYTES)?,
        };
        self.keep_index_under(&inputs, tier, stamp);
        Ok(Some(inputs))
    }

    fn keep_index(&self, inputs: &IndexInputs) {
        if let Some((tier, stamp)) = self.index_tier() {
            self.keep_index_under(inputs, tier, stamp);
        }
    }

    fn keep_index_under(&self, inputs: &IndexInputs, tier: &TempCacheTier, stamp: [u8; 32]) {
        let entry = IndexEntry {
            collection: self.cache_name(),
            collection_stamp: stamp,
            snapshot: inputs.snapshot.clone(),
            log: inputs.log.clone(),
        };
        let admitted = entry.fits() && tier.admits(entry.stored_bytes());
        let key = self.index_key().filter(|_| admitted);
        let Some(key) = key else {
            return;
        };
        let pending = PendingEntry {
            key,
            stamp,
            bytes: entry.encode(),
        };
        self.window.defer(pending, tier.pending_limit());
    }
}
