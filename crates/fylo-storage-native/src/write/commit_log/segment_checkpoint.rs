use std::path::Path;

use fylo_format::{LogRecord, LogRecordKind};

use super::super::{GenerationRecord, durable_replace_owner_only, failpoint, json_error};
use super::log_checkpoint::LogCheckpoint;
use super::log_segment::LogSegment;
use crate::segments::{
    GenerationSlots, LocationTable, SegmentCheckpointManifest, SegmentPaths, SegmentStore,
    SlotRecord, TableCache,
};
use crate::{GenerationState, NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Makes a segment collection's derived files durable and records how far
/// they reach (ADR 0015 section 8).
///
/// It runs under the collection lock with the active segment open. The
/// documents themselves are already durable in their segments, so only the
/// location log, the index log and their snapshots are flushed. When the
/// active segment has grown large it is sealed and the next one opens with
/// the checkpoint record.
pub(crate) struct SegmentCheckpoint;

impl SegmentCheckpoint {
    const ROTATE_BYTES: u64 = 64 * 1024 * 1024;
    const LOCATIONS_FOLD_BYTES: u64 = 64 * 1024;
    const CHECKPOINT_RECORDS: u64 = 4_096;

    /// Whether the collection is due a checkpoint.
    pub(crate) fn wanted(
        collection: &NativeCollection,
        segment: &LogSegment,
    ) -> Result<bool, NativeStorageError> {
        let paths = SegmentPaths::of(&collection.path);
        Ok(segment.records() >= Self::CHECKPOINT_RECORDS
            || segment.length() >= Self::ROTATE_BYTES
            || SegmentStore::file_length(&paths.locations_wal())? > Self::LOCATIONS_FOLD_BYTES
            || LogCheckpoint::index_wants_compaction(collection)?)
    }

    /// Checkpoint at the stable `generation` just published. Returns the
    /// segment that is active afterwards.
    pub(crate) fn run(
        collection: &NativeCollection,
        segment: LogSegment,
        generation: u64,
    ) -> Result<LogSegment, NativeStorageError> {
        let seal = segment.length() >= Self::ROTATE_BYTES;
        Self::run_with(collection, segment, (generation, seal))
    }

    /// Checkpoint and seal the active segment whatever its size, so a
    /// compaction can take in everything written so far.
    pub(crate) fn seal(
        collection: &NativeCollection,
        segment: LogSegment,
        generation: u64,
    ) -> Result<LogSegment, NativeStorageError> {
        Self::run_with(collection, segment, (generation, true))
    }

    fn run_with(
        collection: &NativeCollection,
        segment: LogSegment,
        (generation, seal): (u64, bool),
    ) -> Result<LogSegment, NativeStorageError> {
        let paths = SegmentPaths::of(&collection.path);
        let mut epoch = GenerationSlots::at(&paths.slots()).require()?.table_epoch;
        let fold = SegmentStore::file_length(&paths.locations_wal())? > Self::LOCATIONS_FOLD_BYTES;
        let mut generation = generation;
        if fold {
            let writing = GenerationRecord::log_writing(generation + 1, segment.position());
            Self::publish(collection, &writing, epoch)?;
            generation += 2;
            epoch = epoch.wrapping_add(1);
            Self::fold_locations(&paths)?;
        }
        Self::flush_logs(collection, &paths)?;
        LogCheckpoint::compact_index(collection)?;
        failpoint("after-checkpoint-flush")?;
        let manifest = Self::manifest(&collection.path)?;
        let mut segment = Self::record(&paths, segment, &manifest, (generation, seal))?;
        segment.reset_records();
        let stable = GenerationRecord::log_stable(generation, segment.position());
        Self::publish(collection, &stable, epoch)?;
        TableCache::shared().forget(&paths);
        Ok(segment)
    }

    /// Publish a generation record as a slot, keeping or bumping the table
    /// epoch and recording the location log's current length.
    pub(crate) fn publish(
        collection: &NativeCollection,
        record: &GenerationRecord,
        epoch: u32,
    ) -> Result<(), NativeStorageError> {
        let paths = SegmentPaths::of(&collection.path);
        let position = record.log.as_ref().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "a segment collection publishes without its segment position",
            )
        })?;
        let slot = SlotRecord {
            generation: record.generation,
            writing: record.state == "writing",
            segment: u64::from_str_radix(&position.nonce, 16).map_err(|_| {
                NativeStorageError::new(
                    NativeStorageErrorCode::CorruptMetadata,
                    "segment position names no segment",
                )
            })?,
            length: position.length,
            next_lsn: position.next_lsn,
            records: position.records,
            locations_length: SegmentStore::file_length(&paths.locations_wal())?,
            table_epoch: epoch,
        };
        GenerationSlots::at(&paths.slots()).publish(&slot)
    }

    /// Publish keeping the current table epoch.
    pub(crate) fn publish_current(
        collection: &NativeCollection,
        record: &GenerationRecord,
    ) -> Result<(), NativeStorageError> {
        let paths = SegmentPaths::of(&collection.path);
        let epoch = GenerationSlots::at(&paths.slots())
            .read()?
            .map_or(0, |slot| slot.table_epoch);
        Self::publish(collection, record, epoch)
    }

    /// Publish `writing` at `generation + 1` with a new table epoch, before a
    /// rewrite of the location files that is not an append: a recovery's
    /// cut-back or a rebuild. A cached table can then never be extended past
    /// rows that no longer exist. Returns the generation the closing
    /// checkpoint publishes.
    pub(crate) fn bump_epoch(
        collection: &NativeCollection,
        segment: &LogSegment,
        generation: u64,
    ) -> Result<u64, NativeStorageError> {
        let paths = SegmentPaths::of(&collection.path);
        let epoch = GenerationSlots::at(&paths.slots())
            .read()?
            .map_or(0, |slot| slot.table_epoch);
        let writing = GenerationRecord::log_writing(generation + 1, segment.position());
        Self::publish(collection, &writing, epoch.wrapping_add(1))?;
        TableCache::shared().forget(&paths);
        Ok(generation + 2)
    }

    /// The generation a stable slot should publish after `stored`.
    pub(crate) fn next_stable(stored: &GenerationState, seen: u64) -> u64 {
        let generation = stored.generation.max(seen);
        generation + 2 - generation % 2
    }

    fn fold_locations(paths: &SegmentPaths) -> Result<(), NativeStorageError> {
        let wal_length = SegmentStore::file_length(&paths.locations_wal())?;
        let table = LocationTable::load(paths, wal_length)?;
        durable_replace_owner_only(&paths.locations_snapshot(), &table.encode_snapshot()?)?;
        durable_replace_owner_only(&paths.locations_wal(), b"")
    }

    fn flush_logs(
        collection: &NativeCollection,
        paths: &SegmentPaths,
    ) -> Result<(), NativeStorageError> {
        let index = collection.path.join("index");
        for path in [paths.locations_wal(), index.join("keys.wal")] {
            Self::flush_file(&path)?;
        }
        Ok(())
    }

    fn flush_file(path: &Path) -> Result<(), NativeStorageError> {
        if !crate::path_exists_no_follow(path)? {
            return Ok(());
        }
        let file = fylo_vfs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(NativeStorageError::io)?;
        crate::sync_handle(&file).map_err(NativeStorageError::io)
    }

    /// The checkpoint manifest describing the derived files of the
    /// collection in `directory` as they are now.
    pub(crate) fn manifest(
        directory: &Path,
    ) -> Result<SegmentCheckpointManifest, NativeStorageError> {
        let paths = SegmentPaths::of(directory);
        let index = directory.join("index");
        Ok(SegmentCheckpointManifest {
            format: SegmentCheckpointManifest::FORMAT.into(),
            locations_wal_length: SegmentStore::file_length(&paths.locations_wal())?,
            locations_snapshot_sha256: Self::digest(&paths.locations_snapshot())?,
            keys_wal_length: SegmentStore::file_length(&index.join("keys.wal"))?,
            snapshot_sha256: Self::digest(&index.join("keys.snapshot"))?,
        })
    }

    pub(crate) fn digest(path: &Path) -> Result<String, NativeStorageError> {
        match fylo_vfs::read(path) {
            Ok(bytes) => Ok(crate::sha256_hex(&bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(crate::sha256_hex(b""))
            }
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    /// Append the checkpoint record, or seal the segment and open the next
    /// one with it.
    fn record(
        paths: &SegmentPaths,
        mut segment: LogSegment,
        manifest: &SegmentCheckpointManifest,
        (generation, seal): (u64, bool),
    ) -> Result<LogSegment, NativeStorageError> {
        let bytes = serde_json::to_vec(manifest).map_err(|error| json_error(&error))?;
        if seal {
            return Self::rotate(paths, &segment, bytes, generation);
        }
        let record = LogRecord {
            lsn: segment.next_lsn(),
            kind: LogRecordKind::Checkpoint,
            generation,
            manifest: bytes,
            blobs: Vec::new(),
        };
        let encoded = segment.encode(&record)?;
        segment.append(&encoded, 1)?;
        segment.full_flush()?;
        Ok(segment)
    }

    fn rotate(
        paths: &SegmentPaths,
        sealed: &LogSegment,
        manifest: Vec<u8>,
        generation: u64,
    ) -> Result<LogSegment, NativeStorageError> {
        let number = paths.segment_numbers()?.last().copied().unwrap_or(0) + 1;
        let mut next = LogSegment::create_documents(
            &paths.segment(number),
            (number, sealed.next_lsn()),
            manifest,
            generation,
        )?;
        next.full_flush()?;
        Ok(next)
    }
}
