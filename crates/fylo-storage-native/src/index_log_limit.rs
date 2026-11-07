use crate::{NativeStorageError, NativeStorageErrorCode};
use fylo_query::QueryLimits;

/// The sizes that bound a file-layout collection's index log, `keys.wal`.
///
/// Readers refuse a log over [`Self::READ_BYTES`]. Writers fold the log into
/// `keys.snapshot` once it outgrows the snapshot, and never later than
/// [`Self::COMPACT_CEILING_BYTES`], a quarter of the read limit, so a log
/// stays readable however large the snapshot grows. Compaction reads the log
/// with the snapshot's own limit, so it also folds a log that a release
/// without the ceiling let pass the read limit.
pub(crate) struct IndexLogLimit;

impl IndexLogLimit {
    const BYTES_PER_KIBIBYTE: u64 = 1024;
    const BYTES_PER_MEBIBYTE: u64 = Self::BYTES_PER_KIBIBYTE * Self::BYTES_PER_KIBIBYTE;
    const READ_LIMIT_SHARES: u64 = 4;

    /// The largest log a reader accepts.
    pub(crate) const READ_BYTES: u64 = 64 * Self::BYTES_PER_MEBIBYTE;
    /// A log this small is never worth folding.
    pub(crate) const COMPACT_FLOOR_BYTES: u64 = 64 * Self::BYTES_PER_KIBIBYTE;
    /// A log this large is always folded.
    pub(crate) const COMPACT_CEILING_BYTES: u64 = Self::READ_BYTES / Self::READ_LIMIT_SHARES;

    /// Whether a log of `log_bytes` beside a snapshot of `snapshot_bytes`
    /// is due to be folded into the snapshot.
    pub(crate) fn wants_compaction(log_bytes: u64, snapshot_bytes: u64) -> bool {
        log_bytes > Self::compact_threshold(snapshot_bytes)
    }

    /// The largest log that compaction reads: as large as a snapshot may be.
    pub(crate) fn compaction_read_bytes() -> u64 {
        QueryLimits::default().max_snapshot_bytes as u64
    }

    /// Say how to recover when a reader refuses an oversized log.
    pub(crate) fn explain_refusal(error: NativeStorageError) -> NativeStorageError {
        if error.code() != NativeStorageErrorCode::FileTooLarge {
            return error;
        }
        NativeStorageError::new(
            NativeStorageErrorCode::FileTooLarge,
            format!(
                "{}; the next write to the collection, or recovering it, compacts the index log",
                error.message()
            ),
        )
    }

    fn compact_threshold(snapshot_bytes: u64) -> u64 {
        snapshot_bytes.clamp(Self::COMPACT_FLOOR_BYTES, Self::COMPACT_CEILING_BYTES)
    }
}

#[cfg(test)]
#[path = "index_log_limit/tests.rs"]
mod tests;
