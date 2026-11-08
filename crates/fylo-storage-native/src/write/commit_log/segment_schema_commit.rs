use fylo_format::{LogRecord, LogRecordKind};

use super::super::{GenerationRecord, NativeWriteRoot, failpoint, json_error};
use super::abort_manifest::AbortManifest;
use super::log_segment::LogSegment;
use super::placed_record::PlacedRecord;
use super::segment_checkpoint::SegmentCheckpoint;
use super::segment_schemas::{SchemaVersionManifest, SegmentSchemas};
use crate::{
    GenerationStatus, MAX_SCHEMA_VERSION_BYTES, NativeCollection, NativeStorageError,
    NativeStorageErrorCode,
};

/// Commits a schema version to a segment collection as a record of kind 5
/// (ADR 0017 section 3), through the collection's commit path: under the
/// collection lock, `writing` is published, the record is appended to the
/// active segment and flushed (the commit point), the schemas snapshot is
/// rewritten, `stable` is published, and a checkpoint records the new
/// snapshot. A schema change is therefore ordered against every document
/// commit around it and replayed by recovery.
///
/// The collection may live in a `.vdbx` file, whose superblock first takes
/// the read-only-compatible schemas feature, durably, so a build that
/// predates schema records opens the file read-only.
///
/// A failure before the flush cuts the record back off. A failure after it,
/// in this process, cancels the record with an abort record and puts the
/// snapshot back, so the collection is left at its previous head with
/// nothing for recovery to do. A crash at any point leaves recovery either
/// the previous head (no durable record) or the new one (a durable record
/// that recovery replays).
impl NativeWriteRoot {
    pub(crate) fn commit_segment_schema(
        &self,
        collection: &NativeCollection,
        ordinal: u32,
        version: &[u8],
        verify: impl FnOnce(&NativeCollection) -> Result<(), NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        let medium = collection.segment_medium()?;
        let schemas = SegmentSchemas::load(&*medium, collection.schema_head())?;
        let expected = schemas.head().unwrap_or(0).saturating_add(1);
        if ordinal != expected {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::ConcurrentWrite,
                format!(
                    "schema registry of {} moved: version {ordinal} no longer follows its head",
                    collection.name()
                ),
            ));
        }
        Self::require_storable(version)?;
        verify(collection)?;
        let stored = collection.stored_generation()?;
        if stored.state != GenerationStatus::Stable {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::ConcurrentWrite,
                "segment collection is still writing after recovery",
            ));
        }
        // A `.vdbx` file says it holds schema records before it does.
        medium.mark_schemas()?;
        let mut active = Self::open_active_segment(collection, &stored)?;
        let generation = stored.generation + 2;
        let writing = GenerationRecord::log_writing(stored.generation + 1, active.position());
        SegmentCheckpoint::publish_current(collection, &writing)?;
        let record = LogRecord {
            lsn: active.next_lsn(),
            kind: LogRecordKind::Schema,
            generation,
            manifest: serde_json::to_vec(&SchemaVersionManifest::new(ordinal, version))
                .map_err(|error| json_error(&error))?,
            blobs: version.to_vec(),
        };
        let placed = Self::append_schema_record(collection, &mut active, &record, generation)?;
        let previous = SegmentSchemas::snapshot_bytes(&*medium)?;
        let applied = failpoint("after-schema-record")
            .and_then(|()| {
                SegmentSchemas::entry_of((placed.segment, placed.offset, placed.length), &record)
            })
            .and_then(|entry| schemas.with(entry))
            .and_then(|next| next.write(&*medium))
            .and_then(|()| failpoint("after-schemas-snapshot"));
        if let Err(error) = applied {
            return Err(Self::roll_back_schema(
                collection,
                &mut active,
                (record.lsn, generation),
                previous.as_deref(),
                error,
            ));
        }
        let stable = GenerationRecord::log_stable(generation, active.position());
        SegmentCheckpoint::publish_current(collection, &stable)?;
        // The record is committed; the descriptor follows it for the
        // releases and tools that read the head from there.
        self.write_schema_head(collection, ordinal)?;
        SegmentCheckpoint::run(collection, active, generation)?;
        Ok(())
    }

    fn require_storable(version: &[u8]) -> Result<(), NativeStorageError> {
        if version.len() as u64 > MAX_SCHEMA_VERSION_BYTES {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "schema version exceeds its size limit",
            ));
        }
        if std::str::from_utf8(version).is_err() {
            return Err(NativeStorageError::codec_rejected(
                "a schema version must be UTF-8 JSON",
            ));
        }
        Ok(())
    }

    /// Append and flush the schema record. A failure cuts it back off and
    /// publishes the previous state again as `stable`.
    fn append_schema_record(
        collection: &NativeCollection,
        active: &mut LogSegment,
        record: &LogRecord,
        generation: u64,
    ) -> Result<PlacedRecord, NativeStorageError> {
        let before = (active.length(), active.next_lsn(), active.records());
        let appended = active.encode(record).and_then(|encoded| {
            let placed = PlacedRecord {
                segment: active.sequence(),
                offset: active.length(),
                length: u32::try_from(encoded.len()).map_err(|_| {
                    NativeStorageError::new(
                        NativeStorageErrorCode::FileTooLarge,
                        "schema record exceeds the segment record limit",
                    )
                })?,
                lsn: record.lsn,
            };
            active.append(&encoded, 1)?;
            active.full_flush()?;
            Ok(placed)
        });
        if appended.is_err() {
            active.rewind(before)?;
            let stable = GenerationRecord::log_stable(generation, active.position());
            SegmentCheckpoint::publish_current(collection, &stable)?;
        }
        appended
    }

    /// Cancel a schema record that is durable but whose snapshot could not
    /// be written: an abort record names it, the snapshot is put back and
    /// `stable` is published. Returns the error to report: `error` itself,
    /// or, when the rollback fails too, one that says recovery will finish
    /// the commit.
    fn roll_back_schema(
        collection: &NativeCollection,
        active: &mut LogSegment,
        (lsn, generation): (u64, u64),
        previous: Option<&[u8]>,
        error: NativeStorageError,
    ) -> NativeStorageError {
        let rolled_back = (|| {
            let manifest =
                serde_json::to_vec(&AbortManifest::new(lsn)).map_err(|error| json_error(&error))?;
            let abort = LogRecord {
                lsn: active.next_lsn(),
                kind: LogRecordKind::Abort,
                generation,
                manifest,
                blobs: Vec::new(),
            };
            let encoded = active.encode(&abort)?;
            active.append(&encoded, 1)?;
            active.full_flush()?;
            SegmentSchemas::restore(&*collection.segment_medium()?, previous)?;
            let stable = GenerationRecord::log_stable(generation, active.position());
            SegmentCheckpoint::publish_current(collection, &stable)
        })();
        match rolled_back {
            Ok(()) => error,
            Err(rollback) => NativeStorageError::new(
                NativeStorageErrorCode::Io,
                format!(
                    "schema commit failed ({error}) and rollback failed ({rollback}); recovery will settle the schema registry"
                ),
            ),
        }
    }
}
