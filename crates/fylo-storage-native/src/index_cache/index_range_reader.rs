use super::index_key_set::IndexKeySet;
use super::sorted_key_reader::SortedKeyReader;
use crate::NativeStorageError;
use fylo_query::{IndexKeyRange, IndexSnapshot, QueryLimits};
use std::io::{Read, Seek};

/// Reads only the prefix-index keys in a set of key ranges: the matching runs
/// of the sorted snapshot, and the log lines whose key lies in a range.
///
/// The result equals the full merged index with every other key removed, so
/// its cost follows the size of the ranges and the log, not of the snapshot.
pub(crate) struct IndexRangeReader<'a> {
    ranges: &'a [IndexKeyRange],
}

impl<'a> IndexRangeReader<'a> {
    pub(crate) fn new(ranges: &'a [IndexKeyRange]) -> Self {
        Self { ranges }
    }

    /// Merge the snapshot's keys in the ranges with the log's operations on
    /// them.
    ///
    /// # Errors
    ///
    /// Returns an I/O error, or a corrupt-index error for a malformed run of
    /// snapshot keys.
    pub(crate) fn read<R: Read + Seek>(
        &self,
        (snapshot, length): (R, u64),
        log: &[u8],
    ) -> Result<IndexSnapshot, NativeStorageError> {
        let reader = SortedKeyReader::new(snapshot, length, QueryLimits::default().max_key_bytes);
        let mut keys = self.snapshot_keys(reader)?;
        keys.apply_log(log, &|key| self.contains(key));
        keys.into_snapshot()
    }

    fn snapshot_keys<R: Read + Seek>(
        &self,
        mut reader: SortedKeyReader<R>,
    ) -> Result<IndexKeySet, NativeStorageError> {
        let mut keys = IndexKeySet::new();
        for range in self.ranges {
            keys.insert_keys(reader.keys_in(range)?);
        }
        Ok(keys)
    }

    fn contains(&self, key: &[u8]) -> bool {
        self.ranges.iter().any(|range| range.contains(key))
    }
}
