use crate::{NativeStorageError, NativeStorageErrorCode};
use fylo_query::{IndexSnapshot, QueryLimits};
use std::collections::BTreeSet;

/// Prefix-index keys being merged from a snapshot and its write-ahead log.
///
/// Each complete log line is `+<TAB>key` or `-<TAB>key`. A torn last line is
/// ignored, and so is any line that is not an operation.
pub(super) struct IndexKeySet {
    keys: BTreeSet<Vec<u8>>,
}

impl IndexKeySet {
    const LINE_END: u8 = b'\n';
    const OPERATION_POSITION: usize = 0;
    const SEPARATOR_POSITION: usize = 1;
    const KEY_POSITION: usize = 2;
    const RECORD_SEPARATOR: u8 = b'\t';
    const INSERT: u8 = b'+';
    const REMOVE: u8 = b'-';

    pub(super) fn new() -> Self {
        Self {
            keys: BTreeSet::new(),
        }
    }

    /// Add every key of newline-delimited snapshot bytes.
    pub(super) fn insert_snapshot(&mut self, snapshot: &[u8]) {
        for key in snapshot.split(|byte| *byte == Self::LINE_END) {
            self.insert_nonempty_key(key);
        }
    }

    pub(super) fn insert_keys(&mut self, keys: Vec<Vec<u8>>) {
        self.keys.extend(keys);
    }

    /// Apply the complete log lines whose key `wanted` accepts, in order.
    pub(super) fn apply_log(&mut self, log: &[u8], wanted: &dyn Fn(&[u8]) -> bool) {
        let complete = &log[..Self::complete_log_length(log)];
        for line in complete.split(|byte| *byte == Self::LINE_END) {
            self.apply_wanted_line(line, wanted);
        }
    }

    pub(super) fn into_snapshot(self) -> Result<IndexSnapshot, NativeStorageError> {
        let required = Self::required_bytes(&self.keys)?;
        let merged = Self::serialize_keys(self.keys, required);
        Self::read_merged_snapshot(&merged)
    }

    fn insert_nonempty_key(&mut self, key: &[u8]) {
        if !key.is_empty() {
            self.keys.insert(key.to_vec());
        }
    }

    fn complete_log_length(log: &[u8]) -> usize {
        log.iter()
            .rposition(|byte| *byte == Self::LINE_END)
            .map_or(0, |position| position + 1)
    }

    fn apply_wanted_line(&mut self, line: &[u8], wanted: &dyn Fn(&[u8]) -> bool) {
        if let Some((operation, key)) = Self::log_operation(line)
            && wanted(key)
        {
            self.apply_operation(operation, key);
        }
    }

    fn log_operation(line: &[u8]) -> Option<(u8, &[u8])> {
        line.get(Self::KEY_POSITION)?;
        if line[Self::SEPARATOR_POSITION] != Self::RECORD_SEPARATOR {
            return None;
        }
        Some((line[Self::OPERATION_POSITION], &line[Self::KEY_POSITION..]))
    }

    fn apply_operation(&mut self, operation: u8, key: &[u8]) {
        match operation {
            Self::INSERT => {
                self.keys.insert(key.to_vec());
            }
            Self::REMOVE => {
                self.keys.remove(key);
            }
            _ => {}
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
}
