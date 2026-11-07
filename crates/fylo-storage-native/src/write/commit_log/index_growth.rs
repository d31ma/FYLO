use std::collections::BTreeSet;

use super::log_checkpoint::LogCheckpoint;
use crate::index_limit::IndexLimit;
use crate::{NativeCollection, NativeStorageError};

/// The prefix-index bytes one write adds, and whether its collection has
/// room for them.
///
/// The merged index is never larger than `keys.snapshot` plus every key that
/// `keys.wal` adds, so the two files' lengths together bound it. A write
/// whose removed keys take at least as many bytes as its added keys cannot
/// grow the index, so it always has room: a delete, or an update that
/// rewrites a value in place, still works on a full collection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct IndexGrowth {
    added_key_bytes: u64,
    removed_key_bytes: u64,
    log_bytes_needed: u64,
}

impl IndexGrowth {
    const KEY_LINE_END_BYTES: u64 = 1;
    const LOG_LINE_PREFIX_BYTES: u64 = 2;

    /// The growth of a write that removes `removed` keys and adds `added`.
    pub(crate) fn of(removed: &BTreeSet<String>, added: &BTreeSet<String>) -> Self {
        let added_key_bytes = Self::snapshot_bytes(added);
        Self {
            added_key_bytes,
            removed_key_bytes: Self::snapshot_bytes(removed),
            log_bytes_needed: added_key_bytes + Self::LOG_LINE_PREFIX_BYTES * added.len() as u64,
        }
    }

    /// Refuse the write when it could take the collection's index past the
    /// write limit. A medium that keeps the index itself is not bounded here.
    ///
    /// # Errors
    ///
    /// Returns [`crate::NativeStorageErrorCode::IndexFull`] when the write
    /// does not fit, or an I/O error reading the index files' lengths.
    pub(crate) fn require_room(
        self,
        collection: &NativeCollection,
    ) -> Result<(), NativeStorageError> {
        let needed_bytes = self.needed_bytes();
        let index_bytes = Self::index_file_bytes(collection, needed_bytes)?;
        IndexLimit::require_room(collection.name(), index_bytes, needed_bytes)
    }

    /// The log bytes the write's added keys take, or nothing when the write
    /// cannot grow the index.
    fn needed_bytes(self) -> u64 {
        if self.added_key_bytes <= self.removed_key_bytes {
            return 0;
        }
        self.log_bytes_needed
    }

    /// The bytes of `keys.snapshot` and `keys.wal` together, or nothing when
    /// no bytes are needed or the medium keeps the index.
    fn index_file_bytes(
        collection: &NativeCollection,
        needed_bytes: u64,
    ) -> Result<u64, NativeStorageError> {
        if needed_bytes == 0 || collection.index_in_medium()? {
            return Ok(0);
        }
        let index = collection.path.join("index");
        let snapshot_bytes = LogCheckpoint::length(&index.join("keys.snapshot"))?;
        let log_bytes = LogCheckpoint::length(&index.join("keys.wal"))?;
        Ok(snapshot_bytes.saturating_add(log_bytes))
    }

    fn snapshot_bytes(keys: &BTreeSet<String>) -> u64 {
        let mut total = 0_u64;
        for key in keys {
            total += key.len() as u64 + Self::KEY_LINE_END_BYTES;
        }
        total
    }
}

#[cfg(test)]
#[path = "index_growth/tests.rs"]
mod tests;
