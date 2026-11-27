use super::IndexLimit;
use crate::{NativeStorageError, NativeStorageErrorCode};

const MEBIBYTE: u64 = 1024 * 1024;

#[test]
fn a_log_within_the_floor_is_left_alone() {
    assert!(!IndexLimit::wants_compaction(
        IndexLimit::COMPACT_FLOOR_BYTES,
        0
    ));
}

#[test]
fn a_log_past_the_floor_is_folded_beside_an_empty_snapshot() {
    assert!(IndexLimit::wants_compaction(
        IndexLimit::COMPACT_FLOOR_BYTES + 1,
        0
    ));
}

#[test]
fn a_log_smaller_than_its_snapshot_is_left_alone() {
    assert!(!IndexLimit::wants_compaction(MEBIBYTE, 2 * MEBIBYTE));
}

#[test]
fn a_log_larger_than_its_snapshot_is_folded() {
    assert!(IndexLimit::wants_compaction(2 * MEBIBYTE + 1, 2 * MEBIBYTE));
}

#[test]
fn a_log_past_the_ceiling_is_folded_beside_a_snapshot_over_the_read_limit() {
    assert!(IndexLimit::wants_compaction(
        IndexLimit::COMPACT_CEILING_BYTES + 1,
        200 * MEBIBYTE
    ));
}

#[test]
fn a_log_at_the_ceiling_is_left_alone_beside_a_snapshot_over_the_read_limit() {
    assert!(!IndexLimit::wants_compaction(
        IndexLimit::COMPACT_CEILING_BYTES,
        200 * MEBIBYTE
    ));
}

#[test]
fn the_ceiling_leaves_three_quarters_of_the_read_limit_for_one_commit() {
    assert_eq!(
        IndexLimit::LOG_READ_BYTES - IndexLimit::COMPACT_CEILING_BYTES,
        48 * MEBIBYTE
    );
}

#[test]
fn compaction_reads_a_log_as_large_as_a_snapshot() {
    assert_eq!(IndexLimit::compaction_read_bytes(), 256 * MEBIBYTE);
}

#[test]
fn a_refused_log_says_how_to_recover() {
    let refused = NativeStorageError::new(
        NativeStorageErrorCode::FileTooLarge,
        "keys.wal contains 91 MB; limit is 64 MiB",
    );
    let explained = IndexLimit::explain_refusal(refused);
    assert_eq!(explained.code(), NativeStorageErrorCode::FileTooLarge);
    assert!(explained.message().starts_with("keys.wal contains 91 MB"));
    assert!(explained.message().contains("compacts the index log"));
}

#[test]
fn other_read_errors_pass_through_unchanged() {
    let missing = NativeStorageError::new(NativeStorageErrorCode::NotFound, "gone");
    let passed = IndexLimit::explain_refusal(missing);
    assert_eq!(passed.code(), NativeStorageErrorCode::NotFound);
    assert_eq!(passed.message(), "gone");
}

#[test]
fn writers_stop_sixteen_mebibytes_below_the_read_limit() {
    assert_eq!(IndexLimit::INDEX_BYTES, 256 * MEBIBYTE);
    assert_eq!(IndexLimit::WRITE_BYTES, 240 * MEBIBYTE);
}

#[test]
fn a_write_that_reaches_the_write_limit_exactly_fits() {
    assert!(IndexLimit::require_room("users", IndexLimit::WRITE_BYTES - 10, 10).is_ok());
}

#[test]
fn a_write_one_byte_past_the_write_limit_is_refused() {
    let refused = IndexLimit::require_room("users", IndexLimit::WRITE_BYTES - 10, 11).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::IndexFull);
    assert_eq!(refused.code().as_str(), "EINDEX_FULL");
}

#[test]
fn a_refused_write_names_the_collection_the_limit_and_the_way_out() {
    let refused = IndexLimit::require_room("users", IndexLimit::WRITE_BYTES, 1).unwrap_err();
    let message = refused.message();
    assert!(message.contains("collection users is full"));
    assert!(message.contains("251658241 bytes"));
    assert!(message.contains("write limit of 251658240 bytes (240 MiB"));
    assert!(message.contains("delete documents"));
}

#[test]
fn a_small_log_is_folded_once_the_index_passes_the_write_limit() {
    assert!(IndexLimit::wants_compaction(
        IndexLimit::COMPACT_FLOOR_BYTES + 1,
        IndexLimit::WRITE_BYTES
    ));
}

#[test]
fn a_log_within_the_floor_is_left_alone_past_the_write_limit() {
    assert!(!IndexLimit::wants_compaction(
        IndexLimit::COMPACT_FLOOR_BYTES,
        IndexLimit::WRITE_BYTES
    ));
}

#[test]
fn a_small_log_is_left_alone_while_the_index_fits_the_write_limit() {
    assert!(!IndexLimit::wants_compaction(
        IndexLimit::COMPACT_FLOOR_BYTES + 1,
        IndexLimit::WRITE_BYTES - IndexLimit::COMPACT_FLOOR_BYTES - 1
    ));
}
