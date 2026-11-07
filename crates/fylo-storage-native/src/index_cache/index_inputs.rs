use super::index_key_set::IndexKeySet;
use crate::{NativeStorageError, NativeStorageErrorCode};
use fylo_query::{IndexSnapshot, QueryLimits};

#[derive(PartialEq)]
pub(crate) struct IndexInputs {
    pub(crate) snapshot: Vec<u8>,
    pub(crate) log: Option<Vec<u8>>,
}

impl IndexInputs {
    pub(super) fn retained_bytes(&self, snapshot: &IndexSnapshot) -> Option<usize> {
        self.snapshot
            .capacity()
            .checked_add(self.log.as_ref().map_or(0, Vec::capacity))?
            .checked_add(snapshot.as_bytes().len())
    }

    pub(crate) fn merge(&self) -> Result<IndexSnapshot, NativeStorageError> {
        let snapshot = Self::read_snapshot(&self.snapshot)?;
        let mut keys = IndexKeySet::new();
        keys.insert_snapshot(snapshot.as_bytes());
        keys.apply_log(self.log.as_deref().unwrap_or_default(), &|_| true);
        keys.into_snapshot()
    }

    fn read_snapshot(bytes: &[u8]) -> Result<IndexSnapshot, NativeStorageError> {
        IndexSnapshot::from_bytes(bytes, QueryLimits::default()).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptIndex,
                format!("prefix index snapshot is corrupt: {error}"),
            )
        })
    }
}
