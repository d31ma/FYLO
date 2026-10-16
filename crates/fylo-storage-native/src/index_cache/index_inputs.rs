use crate::{NativeStorageError, NativeStorageErrorCode};
use fylo_query::{IndexSnapshot, QueryLimits};
use std::collections::BTreeSet;

#[derive(PartialEq)]
pub(crate) struct IndexInputs {
    pub(crate) snapshot: Vec<u8>,
    pub(crate) log: Option<Vec<u8>>,
}

impl IndexInputs {
    const LINE_END: u8 = b'\n';
    const OPERATION_POSITION: usize = 0;
    const SEPARATOR_POSITION: usize = 1;
    const KEY_POSITION: usize = 2;
    const RECORD_SEPARATOR: u8 = b'\t';

    pub(super) fn retained_bytes(&self, snapshot: &IndexSnapshot) -> Option<usize> {
        self.snapshot
            .capacity()
            .checked_add(self.log.as_ref().map_or(0, Vec::capacity))?
            .checked_add(snapshot.as_bytes().len())
    }

    pub(super) fn merge(&self) -> Result<IndexSnapshot, NativeStorageError> {
        let snapshot = Self::read_snapshot(&self.snapshot)?;
        let mut keys = Self::snapshot_keys(&snapshot);
        Self::apply_log(&mut keys, self.log.as_deref().unwrap_or_default());
        let required = Self::required_bytes(&keys)?;
        let merged = Self::serialize_keys(keys, required);
        Self::read_merged_snapshot(&merged)
    }

    fn read_snapshot(bytes: &[u8]) -> Result<IndexSnapshot, NativeStorageError> {
        IndexSnapshot::from_bytes(bytes, QueryLimits::default()).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptIndex,
                format!("prefix index snapshot is corrupt: {error}"),
            )
        })
    }

    fn snapshot_keys(snapshot: &IndexSnapshot) -> BTreeSet<Vec<u8>> {
        let mut keys = BTreeSet::new();
        for key in snapshot.as_bytes().split(|byte| Self::is_newline(*byte)) {
            Self::insert_nonempty_key(&mut keys, key);
        }
        keys
    }

    fn is_newline(byte: u8) -> bool {
        byte == Self::LINE_END
    }

    fn insert_nonempty_key(keys: &mut BTreeSet<Vec<u8>>, key: &[u8]) {
        if !key.is_empty() {
            keys.insert(key.to_vec());
        }
    }

    fn required_bytes(keys: &BTreeSet<Vec<u8>>) -> Result<usize, NativeStorageError> {
        Self::count_bytes(keys).ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "merged prefix index size overflow",
            )
        })
    }

    fn count_bytes(keys: &BTreeSet<Vec<u8>>) -> Option<usize> {
        let mut total = 0_usize;
        for key in keys {
            total = total.checked_add(key.len())?.checked_add(1)?;
        }
        Some(total)
    }

    fn serialize_keys(keys: BTreeSet<Vec<u8>>, required: usize) -> Vec<u8> {
        let mut merged = Vec::with_capacity(required);
        for key in keys {
            merged.extend_from_slice(&key);
            merged.push(Self::LINE_END);
        }
        merged
    }

    fn read_merged_snapshot(bytes: &[u8]) -> Result<IndexSnapshot, NativeStorageError> {
        IndexSnapshot::from_bytes(bytes, QueryLimits::default()).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptIndex,
                format!("merged prefix index is corrupt: {error}"),
            )
        })
    }

    fn apply_log(keys: &mut BTreeSet<Vec<u8>>, log: &[u8]) {
        let complete_length = Self::complete_log_length(log);
        Self::apply_complete_lines(keys, &log[..complete_length]);
    }

    fn complete_log_length(log: &[u8]) -> usize {
        log.iter()
            .rposition(|byte| Self::is_newline(*byte))
            .map_or(0, |position| position + 1)
    }

    fn apply_complete_lines(keys: &mut BTreeSet<Vec<u8>>, log: &[u8]) {
        for line in log.split(|byte| Self::is_newline(*byte)) {
            Self::apply_log_line(keys, line);
        }
    }

    fn apply_log_line(keys: &mut BTreeSet<Vec<u8>>, line: &[u8]) {
        let operation = Self::log_operation(line);
        Self::apply_operation(keys, operation);
    }

    fn log_operation(line: &[u8]) -> Option<(u8, Vec<u8>)> {
        line.get(Self::KEY_POSITION)?;
        if line[Self::SEPARATOR_POSITION] != Self::RECORD_SEPARATOR {
            return None;
        }
        Some((
            line[Self::OPERATION_POSITION],
            line[Self::KEY_POSITION..].to_vec(),
        ))
    }

    fn apply_operation(keys: &mut BTreeSet<Vec<u8>>, operation: Option<(u8, Vec<u8>)>) {
        match operation {
            Some((b'+', key)) => {
                keys.insert(key);
            }
            Some((b'-', key)) => {
                keys.remove(&key);
            }
            _ => {}
        }
    }
}
