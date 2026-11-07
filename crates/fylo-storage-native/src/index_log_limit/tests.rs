use super::IndexLogLimit;
use crate::{NativeStorageError, NativeStorageErrorCode};

const MEBIBYTE: u64 = 1024 * 1024;

#[test]
fn a_log_within_the_floor_is_left_alone() {
    assert!(!IndexLogLimit::wants_compaction(
        IndexLogLimit::COMPACT_FLOOR_BYTES,
        0
    ));
}

#[test]
fn a_log_past_the_floor_is_folded_beside_an_empty_snapshot() {
    assert!(IndexLogLimit::wants_compaction(
        IndexLogLimit::COMPACT_FLOOR_BYTES + 1,
        0
    ));
}

#[test]
fn a_log_smaller_than_its_snapshot_is_left_alone() {
    assert!(!IndexLogLimit::wants_compaction(MEBIBYTE, 2 * MEBIBYTE));
}

#[test]
fn a_log_larger_than_its_snapshot_is_folded() {
    assert!(IndexLogLimit::wants_compaction(
        2 * MEBIBYTE + 1,
        2 * MEBIBYTE
    ));
}

#[test]
fn a_log_past_the_ceiling_is_folded_beside_a_snapshot_over_the_read_limit() {
    assert!(IndexLogLimit::wants_compaction(
        IndexLogLimit::COMPACT_CEILING_BYTES + 1,
        200 * MEBIBYTE
    ));
}

#[test]
fn a_log_at_the_ceiling_is_left_alone_beside_a_snapshot_over_the_read_limit() {
    assert!(!IndexLogLimit::wants_compaction(
        IndexLogLimit::COMPACT_CEILING_BYTES,
        200 * MEBIBYTE
    ));
}

#[test]
fn the_ceiling_leaves_three_quarters_of_the_read_limit_for_one_commit() {
    assert_eq!(
        IndexLogLimit::READ_BYTES - IndexLogLimit::COMPACT_CEILING_BYTES,
        48 * MEBIBYTE
    );
}

#[test]
fn compaction_reads_a_log_as_large_as_a_snapshot() {
    assert_eq!(IndexLogLimit::compaction_read_bytes(), 256 * MEBIBYTE);
}

#[test]
fn a_refused_log_says_how_to_recover() {
    let refused = NativeStorageError::new(
        NativeStorageErrorCode::FileTooLarge,
        "keys.wal contains 91 MB; limit is 64 MiB",
    );
    let explained = IndexLogLimit::explain_refusal(refused);
    assert_eq!(explained.code(), NativeStorageErrorCode::FileTooLarge);
    assert!(explained.message().starts_with("keys.wal contains 91 MB"));
    assert!(explained.message().contains("compacts the index log"));
}

#[test]
fn other_read_errors_pass_through_unchanged() {
    let missing = NativeStorageError::new(NativeStorageErrorCode::NotFound, "gone");
    let passed = IndexLogLimit::explain_refusal(missing);
    assert_eq!(passed.code(), NativeStorageErrorCode::NotFound);
    assert_eq!(passed.message(), "gone");
}
