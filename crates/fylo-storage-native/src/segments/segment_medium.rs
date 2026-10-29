use std::ffi::OsString;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use fylo_format::{LogBodyCodec, LogRecord, PlainBody};

use super::{RecordPosition, SlotRecord};
use crate::{Durability, NativeRoot, NativeStorageError, NativeStorageErrorCode};

/// Where one segment collection keeps its segments, generation slots and
/// location files.
///
/// The commit, recovery, compaction and query code reach storage only
/// through this trait, so the same code runs over a collection directory
/// ([`super::DirectoryMedium`]) and over extents of one file. Segments are
/// named by number and addressed by byte offset; a segment number is never
/// reused.
pub(crate) trait SegmentMedium: Debug + Send + Sync {
    /// What process-wide caches key this collection by.
    fn identity(&self) -> MediumIdentity;

    /// The most bytes one segment may hold.
    fn max_segment_bytes(&self) -> u64;

    /// How record bodies are stored in this medium's frames. A directory
    /// of segment files stores them as is; a `.vdbx` container stores a
    /// codec header and the compressed body (ADR 0016 section 4). Every
    /// frame the segment code writes or reads here goes through it.
    fn frame_bodies(&self) -> Arc<dyn LogBodyCodec> {
        static PLAIN: LazyLock<Arc<dyn LogBodyCodec>> = LazyLock::new(|| Arc::new(PlainBody));
        Arc::clone(&PLAIN)
    }

    /// The numbers of every segment present, in ascending order.
    fn segment_numbers(&self) -> Result<Vec<u64>, NativeStorageError>;

    /// Bytes segment `number` holds, or zero when it does not exist.
    fn segment_length(&self, number: u64) -> Result<u64, NativeStorageError>;

    /// Every byte of segment `number`, or `None` when it does not exist.
    fn read_segment(&self, number: u64) -> Result<Option<Vec<u8>>, NativeStorageError>;

    /// A handle a reader keeps for the duration of a stable read. A segment
    /// that does not exist (compaction removed it) is a moved generation,
    /// which readers retry: `ConcurrentWrite`.
    fn read_handle(&self, number: u64) -> Result<Arc<dyn SegmentHandle>, NativeStorageError>;

    /// A handle the writer appends through, or `None` when segment `number`
    /// does not exist.
    fn write_handle(
        &self,
        number: u64,
    ) -> Result<Option<Arc<dyn SegmentHandle>>, NativeStorageError>;

    /// Create the next segment, numbered above every existing one, and
    /// return its number with a handle to it. The medium picks the number:
    /// `opening` builds, for that number, the bytes the segment starts with
    /// (its header and first record). Nothing is flushed.
    fn create_segment(
        &self,
        opening: &dyn Fn(u64) -> Result<Vec<u8>, NativeStorageError>,
    ) -> Result<(u64, Arc<dyn SegmentHandle>), NativeStorageError>;

    /// Mark segment `number` finished: no record is appended to it again,
    /// and `live_bytes` of its records are what the location table still
    /// refers to. A medium with an extent map records both there, where
    /// shrinking, `fylo inspect` and `fylo verify` read them; a directory of
    /// segment files has nowhere to keep them.
    fn seal_segment(&self, number: u64, live_bytes: u64) -> Result<(), NativeStorageError>;

    /// Undo [`Self::seal_segment`] for segment `number`, which stays the
    /// active one because the rotation that sealed it failed.
    fn unseal_segment(&self, _number: u64) -> Result<(), NativeStorageError> {
        Ok(())
    }

    /// Remove segment `number`. A segment that is already gone, or that
    /// another process still holds where that prevents removal, is left for
    /// the next recovery or compaction.
    fn remove_segment(&self, number: u64) -> Result<(), NativeStorageError>;

    /// What a caller is told holds segment `number`'s documents. It is
    /// reported, never opened.
    fn segment_path(&self, number: u64) -> PathBuf;

    /// Start this process's compacted copy, replacing any earlier one.
    fn create_compaction(&self) -> Result<Arc<dyn SegmentHandle>, NativeStorageError>;

    /// Every byte of this process's compacted copy.
    fn read_compaction(&self) -> Result<Vec<u8>, NativeStorageError>;

    /// Make this process's compacted copy a segment numbered above every
    /// existing one, durably. Returns its number.
    fn adopt_compaction(&self) -> Result<u64, NativeStorageError>;

    /// Remove every compacted copy an interrupted compaction left. The
    /// caller holds the compaction lock.
    fn remove_compactions(&self) -> Result<(), NativeStorageError>;

    /// Whether a compaction under way elsewhere, in another process or
    /// another thread, may hold a copy that is a segment no location names
    /// yet, so a recovery leaves unreferenced segments to that compaction.
    /// A medium whose copies are not segments until they are adopted has
    /// none to protect.
    fn compaction_running(&self) -> Result<bool, NativeStorageError> {
        Ok(false)
    }

    /// Whether the slot pair exists, which completes a new layout.
    fn has_slots(&self) -> Result<bool, NativeStorageError>;

    /// Create what holds the segments of a new layout, verified beneath
    /// `root`.
    fn create_storage(&self, root: &NativeRoot) -> Result<(), NativeStorageError>;

    /// The newest valid generation slot, or `None` when neither slot is
    /// valid or the slots are missing.
    fn read_slot(&self) -> Result<Option<SlotRecord>, NativeStorageError>;

    /// Overwrite the older slot with `slot`, in place and without a flush.
    fn publish_slot(&self, slot: &SlotRecord) -> Result<(), NativeStorageError>;

    /// Durably create the slot pair holding `slot` in its first slot.
    fn create_slots(&self, slot: &SlotRecord) -> Result<(), NativeStorageError>;

    /// Flush the slots and the names of the segments created so far.
    fn sync_slots(&self) -> Result<(), NativeStorageError>;

    /// The location snapshot, empty when there is none.
    fn read_location_snapshot(&self) -> Result<Vec<u8>, NativeStorageError>;

    /// Durably replace the location snapshot.
    ///
    /// A medium without a location log records with it the end of the
    /// records the newest slot names, which the new snapshot must reach:
    /// readers replay the records from there.
    fn replace_location_snapshot(&self, bytes: &[u8]) -> Result<(), NativeStorageError>;

    /// The schemas snapshot's bytes (ADR 0017 section 3), or `None` when
    /// there is none.
    fn read_schemas_snapshot(&self) -> Result<Option<Vec<u8>>, NativeStorageError>;

    /// What changes whenever the schemas snapshot does, so a reader can
    /// keep it decoded, or `None` when there is none.
    fn schemas_snapshot_stamp(&self) -> Result<Option<SnapshotStamp>, NativeStorageError>;

    /// Durably replace the schemas snapshot.
    fn replace_schemas_snapshot(&self, bytes: &[u8]) -> Result<(), NativeStorageError>;

    /// Put the schemas snapshot back as `previous` held it, `None` meaning
    /// there was none, for a schema commit or compaction being rolled back.
    /// This is a compensating step, so it passes no failpoint.
    fn restore_schemas_snapshot(&self, previous: Option<&[u8]>) -> Result<(), NativeStorageError>;

    /// Record, before the first schema record is written, that the
    /// collection holds schema versions, where its storage has a format
    /// flag for it: a `.vdbx` file sets `READ_ONLY_COMPAT_SCHEMAS` in its
    /// superblock (ADR 0017 section 3), so a build that predates schema
    /// records opens it read-only. A collection directory is fenced by its
    /// segment marker and descriptor instead, and does nothing here.
    fn mark_schemas(&self) -> Result<(), NativeStorageError> {
        Ok(())
    }

    /// Before a compaction copies records, under the collection lock:
    /// train a newer compression dictionary when it stores the newest
    /// records enough smaller, so that the copy and every later record use
    /// it. Returns whether new records now use another dictionary. A
    /// medium that stores bodies as is has no dictionary.
    fn retrain_dictionary(&self) -> Result<bool, NativeStorageError> {
        Ok(false)
    }

    /// After a compaction removed the segments it replaced: release every
    /// compression dictionary no record names any longer.
    fn release_unused_dictionaries(&self) -> Result<(), NativeStorageError> {
        Ok(())
    }

    /// Before records are imported into a new collection, under the
    /// collection lock: train a compression dictionary on `records`, the
    /// first to come, when they hold enough to train one, so that they are
    /// written with it rather than wait for a compaction. Returns whether
    /// it did.
    fn prime_dictionary(&self, _records: &[LogRecord]) -> Result<bool, NativeStorageError> {
        Ok(false)
    }

    /// Before records are imported into a new collection that a sealed pack
    /// will recompress in blocks (ADR 0016 section 8): store every record
    /// body from now on as is, and train no dictionary, since a block
    /// compresses many records at once far better than one record
    /// compresses alone. A directory medium stores bodies as is already.
    fn write_uncompressed(&self) {}

    /// Whether location rows go to a location log. A medium without one
    /// (a `.vdbx` container) keeps them only in memory: a location table is
    /// its snapshot and the rows the records after the snapshot's position
    /// derive (`docs/vdbx-segment-backend.md`), and every location log
    /// method does nothing or reports an empty log.
    fn has_location_log(&self) -> bool;

    /// The location snapshot and the record position it reaches, read
    /// together, for a medium without a location log.
    fn read_location_checkpoint(&self) -> Result<(Vec<u8>, RecordPosition), NativeStorageError>;

    /// Bytes in the location log, or zero when there is none.
    fn location_log_length(&self) -> Result<u64, NativeStorageError>;

    /// The location log's bytes from `start` to `end`.
    fn read_location_log(&self, start: u64, end: u64) -> Result<Vec<u8>, NativeStorageError>;

    /// Append rows to the location log, without a flush.
    fn append_location_log(&self, bytes: &[u8]) -> Result<(), NativeStorageError>;

    /// Cut the location log back to `length` bytes, creating it if needed.
    fn truncate_location_log(&self, length: u64) -> Result<(), NativeStorageError>;

    /// Durably replace the location log with an empty one.
    fn reset_location_log(&self) -> Result<(), NativeStorageError>;

    /// Flush the location log, if there is one.
    fn sync_location_log(&self) -> Result<(), NativeStorageError>;

    /// The owner and group a file this collection's writer creates gets.
    #[cfg(unix)]
    fn owner(&self) -> Result<(u32, u32), NativeStorageError>;

    /// Whether the medium keeps the collection's prefix index itself, as a
    /// `.vdbx` container does: a snapshot beside the location snapshot, and
    /// the keys past it replayed from the records' index deltas
    /// (`docs/vdbx-segment-backend.md`, "Root integration"). A medium that
    /// does not leaves the index to `index/` files in the collection
    /// directory, and never has the index methods below called.
    fn keeps_index(&self) -> bool {
        false
    }

    /// The prefix index as the records up to `to`, a published record
    /// boundary, leave it: the snapshot, and the index log lines the
    /// records after the snapshot's position derive.
    fn read_index(&self, _to: RecordPosition) -> Result<(Vec<u8>, Vec<u8>), NativeStorageError> {
        Err(no_index())
    }

    /// The index snapshot the newest checkpoint names, or `None` when it
    /// names none although it names a location snapshot, or when the
    /// snapshot cannot be read: the index must then be rebuilt.
    fn read_index_snapshot(&self) -> Result<Option<Vec<u8>>, NativeStorageError> {
        Err(no_index())
    }

    /// Durably replace the index snapshot with one a rebuild wrote from the
    /// documents, keeping the newest checkpoint's location snapshot and
    /// position.
    fn replace_index_snapshot(&self, _bytes: &[u8]) -> Result<(), NativeStorageError> {
        Err(no_index())
    }

    /// Every byte of segment `number`, failing when it does not exist.
    fn require_segment(&self, number: u64) -> Result<Vec<u8>, NativeStorageError> {
        self.read_segment(number)?.ok_or_else(|| {
            NativeStorageError::io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("segment {number} does not exist"),
            ))
        })
    }

    /// The newest valid slot, failing when neither is valid.
    fn require_slot(&self) -> Result<SlotRecord, NativeStorageError> {
        self.read_slot()?.ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "segment collection has no valid generation slot",
            )
        })
    }
}

/// What an index method reports on a medium that keeps no index.
fn no_index() -> NativeStorageError {
    NativeStorageError::new(
        NativeStorageErrorCode::Unsupported,
        "this segment medium keeps its prefix index in index/ files",
    )
}

/// One open segment.
///
/// Reads and writes are positioned; the handle keeps no position of its
/// own that callers rely on.
pub(crate) trait SegmentHandle: Debug + Send + Sync {
    /// Bytes the segment holds.
    fn length(&self) -> Result<u64, NativeStorageError>;

    /// Fill `buffer` from `offset`. A segment that ends first means the
    /// generation moved under the reader: `ConcurrentWrite`.
    fn read_at(&self, buffer: &mut [u8], offset: u64) -> Result<(), NativeStorageError>;

    /// Write `bytes` at `offset`.
    fn write_at(&self, bytes: &[u8], offset: u64) -> Result<(), NativeStorageError>;

    /// Cut the segment back to `length` bytes.
    fn truncate(&self, length: u64) -> Result<(), NativeStorageError>;

    /// Flush the segment's bytes as hard as `durability` asks.
    fn sync(&self, durability: Durability) -> Result<(), NativeStorageError>;

    /// Make a newly created segment's name durable, once its bytes are.
    fn sync_created(&self) -> Result<(), NativeStorageError>;
}

/// What changes whenever a schemas snapshot does
/// ([`SegmentMedium::schemas_snapshot_stamp`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SnapshotStamp {
    /// A snapshot file's length and modification time. The file is only
    /// ever replaced by a rename, so a changed file shows a changed stamp.
    File(u64, Option<std::time::SystemTime>),
    /// A snapshot run's length and the SHA-256 of its stored bytes, as the
    /// checkpoint slot that names it records them.
    Run(u64, [u8; 32]),
}

/// The key process-wide caches hold one segment collection under.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct MediumIdentity(OsString);

impl MediumIdentity {
    pub(crate) fn new(identity: impl Into<OsString>) -> Self {
        Self(identity.into())
    }
}
