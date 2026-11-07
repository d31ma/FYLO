use crate::{NativeStorageError, NativeStorageErrorCode};

/// The sizes that bound a file-layout collection's prefix index: the
/// `keys.snapshot` file, the `keys.wal` log beside it, and the index a
/// reader merges from the two.
///
/// Readers refuse a snapshot or a merged index over [`Self::INDEX_BYTES`],
/// and a log over [`Self::LOG_READ_BYTES`]. Writers fold the log into the
/// snapshot once it outgrows the snapshot, and never later than
/// [`Self::COMPACT_CEILING_BYTES`], a quarter of the log read limit, so a
/// log stays readable however large the snapshot grows. Compaction reads the
/// log with the snapshot's own limit, so it also folds a log that a release
/// without the ceiling let pass the read limit.
///
/// Writers refuse a write that could take the index past
/// [`Self::WRITE_BYTES`], which leaves [`Self::WRITE_HEADROOM_BYTES`] below
/// the read limit, so the index always stays readable and compactable.
pub(crate) struct IndexLimit;

impl IndexLimit {
    const BYTES_PER_KIBIBYTE: u64 = 1024;
    const BYTES_PER_MEBIBYTE: u64 = Self::BYTES_PER_KIBIBYTE * Self::BYTES_PER_KIBIBYTE;
    const READ_LIMIT_SHARES: u64 = 4;

    /// The largest snapshot, and the largest merged index, a reader accepts.
    pub(crate) const INDEX_BYTES: u64 = fylo_query::DEFAULT_MAX_SNAPSHOT_BYTES as u64;
    /// The largest log a reader accepts.
    pub(crate) const LOG_READ_BYTES: u64 = 64 * Self::BYTES_PER_MEBIBYTE;
    /// A log this small is never worth folding.
    pub(crate) const COMPACT_FLOOR_BYTES: u64 = 64 * Self::BYTES_PER_KIBIBYTE;
    /// A log this large is always folded.
    pub(crate) const COMPACT_CEILING_BYTES: u64 = Self::LOG_READ_BYTES / Self::READ_LIMIT_SHARES;
    /// The room writers leave below [`Self::INDEX_BYTES`]. It holds the keys
    /// of commits that check the index before an earlier commit of the same
    /// moment has appended its own, so the index never passes the read
    /// limit and compaction can always fold it.
    pub(crate) const WRITE_HEADROOM_BYTES: u64 = Self::COMPACT_CEILING_BYTES;
    /// The largest index a write may leave behind.
    pub(crate) const WRITE_BYTES: u64 = Self::INDEX_BYTES - Self::WRITE_HEADROOM_BYTES;

    /// Whether a log of `log_bytes` beside a snapshot of `snapshot_bytes`
    /// is due to be folded into the snapshot.
    pub(crate) fn wants_compaction(log_bytes: u64, snapshot_bytes: u64) -> bool {
        log_bytes > Self::compact_threshold(log_bytes, snapshot_bytes)
    }

    /// The largest log that compaction reads: as large as a snapshot may be.
    pub(crate) fn compaction_read_bytes() -> u64 {
        Self::INDEX_BYTES
    }

    /// Refuse a write that needs `needed_bytes` more index beside an index
    /// of at most `index_bytes`, when the two together pass
    /// [`Self::WRITE_BYTES`].
    ///
    /// # Errors
    ///
    /// Returns [`NativeStorageErrorCode::IndexFull`], naming the collection
    /// and the limit, when the write does not fit.
    pub(crate) fn require_room(
        collection_name: &str,
        index_bytes: u64,
        needed_bytes: u64,
    ) -> Result<(), NativeStorageError> {
        let grown_bytes = index_bytes.saturating_add(needed_bytes);
        Self::refuse_past_write_limit(collection_name, grown_bytes)
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

    /// A log is folded once it outgrows its snapshot, within the floor and
    /// the ceiling. Past the write limit, where the log's removals may hide
    /// room that a write needs, it is folded as soon as it passes the floor.
    fn compact_threshold(log_bytes: u64, snapshot_bytes: u64) -> u64 {
        if Self::fits_write_limit(snapshot_bytes.saturating_add(log_bytes)) {
            return snapshot_bytes.clamp(Self::COMPACT_FLOOR_BYTES, Self::COMPACT_CEILING_BYTES);
        }
        Self::COMPACT_FLOOR_BYTES
    }

    fn fits_write_limit(index_bytes: u64) -> bool {
        index_bytes <= Self::WRITE_BYTES
    }

    fn refuse_past_write_limit(
        collection_name: &str,
        grown_bytes: u64,
    ) -> Result<(), NativeStorageError> {
        if Self::fits_write_limit(grown_bytes) {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::IndexFull,
            format!(
                "collection {collection_name} is full: this write could grow its prefix index to {grown_bytes} bytes, past the write limit of {} bytes ({} MiB: the {} MiB index limit less {} MiB kept for compaction); delete documents from it, or put new documents in another collection",
                Self::WRITE_BYTES,
                Self::WRITE_BYTES / Self::BYTES_PER_MEBIBYTE,
                Self::INDEX_BYTES / Self::BYTES_PER_MEBIBYTE,
                Self::WRITE_HEADROOM_BYTES / Self::BYTES_PER_MEBIBYTE,
            ),
        ))
    }
}

#[cfg(test)]
#[path = "index_limit/tests.rs"]
mod tests;
