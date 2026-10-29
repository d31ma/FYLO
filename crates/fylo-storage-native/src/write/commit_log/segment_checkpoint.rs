use std::path::Path;

use fylo_format::{LogRecord, LogRecordKind};

use super::super::{GenerationRecord, failpoint, json_error};
use super::log_checkpoint::LogCheckpoint;
use super::log_segment::LogSegment;
use super::segment_schemas::SegmentSchemas;
use crate::segments::{
    LocationTable, SegmentCheckpointManifest, SegmentMedium, SlotRecord, TableCache, TableReach,
};
use crate::{GenerationState, NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Makes a segment collection's derived files durable and records how far
/// they reach (ADR 0015 section 8).
///
/// It runs under the collection lock with the active segment open. The
/// documents themselves are already durable in their segments, so only the
/// location log, the index log and their snapshots are flushed, and the
/// checkpoint record names the schemas snapshot it found. When the
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
        let medium = collection.segment_medium()?;
        Ok(segment.records() >= Self::CHECKPOINT_RECORDS
            || segment.length() >= Self::rotate_bytes(&*medium)
            || medium.location_log_length()? > Self::LOCATIONS_FOLD_BYTES
            || LogCheckpoint::index_wants_compaction(collection)?)
    }

    /// How long the active segment grows before a checkpoint seals it:
    /// half of what a segment may hold where that is less, so a busy
    /// period's appends still fit after the check.
    fn rotate_bytes(medium: &dyn SegmentMedium) -> u64 {
        Self::ROTATE_BYTES.min(medium.max_segment_bytes() / 2)
    }

    /// Whether this checkpoint rewrites the location snapshot: when the
    /// location log has grown, and always for a medium without one, so
    /// the records a table replays stay bounded by the checkpoint interval.
    fn folds(medium: &dyn SegmentMedium) -> Result<bool, NativeStorageError> {
        if !medium.has_location_log() {
            return Ok(true);
        }
        Ok(medium.location_log_length()? > Self::LOCATIONS_FOLD_BYTES)
    }

    /// Checkpoint at the stable `generation` just published. Returns the
    /// segment that is active afterwards.
    pub(crate) fn run(
        collection: &NativeCollection,
        segment: LogSegment,
        generation: u64,
    ) -> Result<LogSegment, NativeStorageError> {
        let seal = segment.length() >= Self::rotate_bytes(&*collection.segment_medium()?);
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
        let medium = collection.segment_medium()?;
        let mut epoch = medium.require_slot()?.table_epoch;
        let fold = Self::folds(&*medium)?;
        let mut generation = generation;
        let mut table = None;
        if fold {
            let writing = GenerationRecord::log_writing(generation + 1, segment.position());
            Self::publish(collection, &writing, epoch)?;
            generation += 2;
            epoch = epoch.wrapping_add(1);
            table = Some(Self::fold_locations(&*medium)?);
        }
        Self::flush_logs(collection, &*medium)?;
        LogCheckpoint::compact_index(collection)?;
        // A schema commit leaves the snapshot current, so this writes only
        // when it is missing or no longer matches the records.
        SegmentSchemas::load(&*medium, collection.schema_head())?.write_if_changed(&*medium)?;
        failpoint("after-checkpoint-flush")?;
        let manifest = Self::manifest(&collection.path, &*medium)?;
        let mut segment = Self::record(
            &*medium,
            (segment, table.as_ref()),
            &manifest,
            (generation, seal),
        )?;
        segment.reset_records();
        let stable = GenerationRecord::log_stable(generation, segment.position());
        Self::publish(collection, &stable, epoch)?;
        TableCache::shared().forget(&*medium);
        Ok(segment)
    }

    /// Publish a generation record as a slot, keeping or bumping the table
    /// epoch and recording the location log's current length.
    pub(crate) fn publish(
        collection: &NativeCollection,
        record: &GenerationRecord,
        epoch: u32,
    ) -> Result<(), NativeStorageError> {
        let medium = collection.segment_medium()?;
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
            locations_length: medium.location_log_length()?,
            table_epoch: epoch,
        };
        medium.publish_slot(&slot)
    }

    /// Publish keeping the current table epoch.
    pub(crate) fn publish_current(
        collection: &NativeCollection,
        record: &GenerationRecord,
    ) -> Result<(), NativeStorageError> {
        let epoch = collection
            .segment_medium()?
            .read_slot()?
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
        let medium = collection.segment_medium()?;
        let epoch = medium.read_slot()?.map_or(0, |slot| slot.table_epoch);
        let writing = GenerationRecord::log_writing(generation + 1, segment.position());
        Self::publish(collection, &writing, epoch.wrapping_add(1))?;
        TableCache::shared().forget(&*medium);
        Ok(generation + 2)
    }

    /// The generation a stable slot should publish after `stored`.
    pub(crate) fn next_stable(stored: &GenerationState, seen: u64) -> u64 {
        let generation = stored.generation.max(seen);
        generation + 2 - generation % 2
    }

    /// Write the table as the new snapshot, and return it.
    fn fold_locations(medium: &dyn SegmentMedium) -> Result<LocationTable, NativeStorageError> {
        let table = LocationTable::load(medium, TableReach::written(medium)?)?;
        medium.replace_location_snapshot(&table.encode_snapshot()?)?;
        medium.reset_location_log()?;
        Ok(table)
    }

    fn flush_logs(
        collection: &NativeCollection,
        medium: &dyn SegmentMedium,
    ) -> Result<(), NativeStorageError> {
        medium.sync_location_log()?;
        if medium.keeps_index() {
            return Ok(());
        }
        Self::flush_file(&collection.path.join("index").join("keys.wal"))
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
    /// collection in `directory`, whose segments `medium` holds, as they
    /// are now.
    ///
    /// A medium that keeps the index has no index log, and an index it
    /// cannot read is recorded as the empty one, so the next recovery finds
    /// it does not match and rebuilds it instead of the checkpoint failing.
    pub(crate) fn manifest(
        directory: &Path,
        medium: &dyn SegmentMedium,
    ) -> Result<SegmentCheckpointManifest, NativeStorageError> {
        let (keys_wal_length, snapshot_sha256) = if medium.keeps_index() {
            let snapshot = medium.read_index_snapshot()?.unwrap_or_default();
            (0, crate::sha256_hex(&snapshot))
        } else {
            let index = directory.join("index");
            (
                Self::file_length(&index.join("keys.wal"))?,
                Self::digest(&index.join("keys.snapshot"))?,
            )
        };
        Ok(SegmentCheckpointManifest {
            format: SegmentCheckpointManifest::FORMAT.into(),
            locations_wal_length: medium.location_log_length()?,
            locations_snapshot_sha256: crate::sha256_hex(&medium.read_location_snapshot()?),
            keys_wal_length,
            snapshot_sha256,
            schemas_snapshot_sha256: SegmentSchemas::digest(medium)?,
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

    /// A file's length, or zero when it does not exist.
    pub(crate) fn file_length(path: &Path) -> Result<u64, NativeStorageError> {
        match fylo_vfs::metadata(path) {
            Ok(metadata) => Ok(metadata.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    /// Append the checkpoint record, or seal the segment and open the next
    /// one with it. `table` is the table this checkpoint folded, if it
    /// folded one.
    fn record(
        medium: &dyn SegmentMedium,
        (mut segment, table): (LogSegment, Option<&LocationTable>),
        manifest: &SegmentCheckpointManifest,
        (generation, seal): (u64, bool),
    ) -> Result<LogSegment, NativeStorageError> {
        let bytes = serde_json::to_vec(manifest).map_err(|error| json_error(&error))?;
        if seal {
            return Self::rotate(medium, (&segment, table), &bytes, generation);
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

    /// Seal the full segment with the bytes the folded table still refers
    /// to in it, and open the next. A medium without a location log always
    /// folds, so the medium that keeps seals always gets them; a directory
    /// of segment files keeps none.
    fn rotate(
        medium: &dyn SegmentMedium,
        (sealed, table): (&LogSegment, Option<&LocationTable>),
        manifest: &[u8],
        generation: u64,
    ) -> Result<LogSegment, NativeStorageError> {
        if let Some(table) = table {
            medium.seal_segment(sealed.sequence(), table.live_bytes(sealed.sequence()))?;
        }
        let mut next =
            LogSegment::create_documents(medium, sealed.next_lsn(), manifest, generation)?;
        next.full_flush()?;
        Ok(next)
    }
}
