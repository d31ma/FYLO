use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;

use fylo_format::{LogRecord, LogRecordKind, LogSegmentHeader, LogSegmentKind};
use fylo_vfs::OpenOptions;

use super::super::{GenerationRecord, failpoint, json_error, sync_directory};
use super::abort_manifest::AbortManifest;
use super::compaction_manifest::CompactionManifest;
use super::compaction_report::CompactionReport;
use super::log_segment::LogSegment;
use super::segment_checkpoint::SegmentCheckpoint;
use super::segment_plan::SegmentPlan;
use super::segment_recovery::{SchemaCopies, SegmentRecovery};
use super::segment_schemas::{SchemaVersionEntry, SegmentSchemas};
use crate::segments::{
    SegmentCommitManifest, SegmentDocument, SegmentLocation, SegmentPaths, SegmentReader,
    SegmentStore,
};
use crate::{GenerationState, NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Reclaims the space dead entries take in sealed segments (ADR 0015
/// section 7).
///
/// The copy runs without the collection lock, because sealed segments never
/// change. Each live entry keeps its sequence number, so the copy and the
/// original are interchangeable, and the compaction commits by appending one
/// compaction record to the active segment under the lock.
///
/// Schema records (ADR 0017 section 3) are not documents, but they are live
/// for as long as the registry lists them: each one in a chosen segment is
/// copied too, with its sequence number, and the schemas snapshot is pointed
/// at the copy when the compaction is adopted.
pub(crate) struct SegmentCompactor {
    candidates: Vec<u64>,
    rows: Vec<SegmentLocation>,
    schemas: Vec<SchemaVersionEntry>,
    bytes_before: u64,
}

impl SegmentCompactor {
    const SMALL_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;
    const TEMPORARY_PREFIX: &'static str = "compacting-";
    const TEMPORARY_SUFFIX: &'static str = ".tmp";

    /// Choose what to compact. Runs under the collection lock. The active
    /// segment is sealed first when it holds documents, so everything
    /// written so far can be compacted.
    pub(crate) fn prepare(
        collection: &NativeCollection,
        stored: &GenerationState,
        active: LogSegment,
    ) -> Result<Option<Self>, NativeStorageError> {
        let paths = SegmentPaths::of(&collection.path);
        let store = SegmentStore::open_for_writer(&collection.path)?;
        let active_number = active.sequence();
        let holds_documents = store
            .table()
            .rows()
            .iter()
            .any(|row| row.segment == active_number);
        let active = if holds_documents {
            SegmentCheckpoint::seal(collection, active, stored.generation)?
        } else {
            active
        };
        Self::remove_temporaries(&paths)?;
        SegmentRecovery::remove_unreferenced(&paths, active.sequence())?;
        let store = SegmentStore::open_for_writer(&collection.path)?;
        let schemas = SegmentSchemas::load(&paths, collection.schema_head())?;
        Self::choose(&paths, (&store, &schemas), active.sequence())
    }

    /// Copy every live entry of the chosen segments, in identifier order,
    /// into a temporary compacted segment. Runs without the lock.
    pub(crate) fn copy(
        &self,
        collection: &NativeCollection,
    ) -> Result<PathBuf, NativeStorageError> {
        let paths = SegmentPaths::of(&collection.path);
        let path = paths.directory().join(format!(
            "{}{}{}",
            Self::TEMPORARY_PREFIX,
            crate::process_id(),
            Self::TEMPORARY_SUFFIX
        ));
        let reader = SegmentReader::new(paths);
        let header = LogSegmentHeader {
            kind: LogSegmentKind::CompactedDocuments,
            sequence: 0,
            first_lsn: self
                .rows
                .iter()
                .map(|row| row.lsn)
                .chain(self.schemas.iter().map(|entry| entry.lsn))
                .min()
                .unwrap_or(1),
        };
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        LogSegment::owner_only(&mut options, LogSegmentKind::CompactedDocuments);
        let mut file = options.open(&path).map_err(NativeStorageError::io)?;
        file.write_all(&header.encode())
            .map_err(NativeStorageError::io)?;
        for entry in &self.schemas {
            let frame = Self::copied_schema(&reader, entry)?;
            file.write_all(&frame).map_err(NativeStorageError::io)?;
        }
        let mut records: BTreeMap<(u64, u64), LogRecord> = BTreeMap::new();
        for row in &self.rows {
            let key = (row.segment, row.offset);
            if let Entry::Vacant(slot) = records.entry(key) {
                slot.insert(reader.record(row)?);
            }
            let frame = Self::copied_frame(row, &records[&key])?;
            file.write_all(&frame).map_err(NativeStorageError::io)?;
        }
        file.sync_all().map_err(NativeStorageError::io)?;
        failpoint("after-compaction-copy")?;
        Ok(path)
    }

    /// Adopt the copy. Runs under the collection lock: the compaction record
    /// appended to the active segment is the commit point.
    pub(crate) fn adopt(
        &self,
        collection: &NativeCollection,
        (stored, mut active): (&GenerationState, LogSegment),
        copied: &std::path::Path,
    ) -> Result<CompactionReport, NativeStorageError> {
        let paths = SegmentPaths::of(&collection.path);
        let number = paths.segment_numbers()?.last().copied().unwrap_or(0) + 1;
        let adopted = paths.segment(number);
        let sha256 = crate::sha256_hex(&fylo_vfs::read(copied).map_err(NativeStorageError::io)?);
        fylo_vfs::rename(copied, &adopted).map_err(NativeStorageError::io)?;
        sync_directory(paths.directory())?;
        if let Err(error) = failpoint("after-compaction-sync") {
            Self::remove_quietly(&adopted);
            return Err(error);
        }
        let writing = GenerationRecord::log_writing(stored.generation + 1, active.position());
        SegmentCheckpoint::publish_current(collection, &writing)?;
        let lsn = active.next_lsn();
        let generation = stored.generation + 2;
        let before = (active.length(), active.next_lsn(), active.records());
        let recorded =
            Self::append_record(&mut active, (number, sha256), &self.candidates, generation);
        if let Err(error) = recorded {
            Self::cancel(collection, &mut active, (before, generation), &adopted)?;
            return Err(error);
        }
        let store = SegmentStore::open_for_writer(&collection.path)?;
        let (rows, schema_copies) = CompactionManifest::adopted(&paths, store.table(), number)?;
        let previous: Vec<SegmentLocation> = rows
            .iter()
            .filter_map(|row| store.table().get(&row.id).cloned())
            .collect();
        let previous_schemas = SegmentSchemas::snapshot_bytes(&paths)?;
        let applied = failpoint("after-compaction-record")
            .and_then(|()| SegmentPlan::append_locations(&collection.path, &rows))
            .and_then(|()| Self::relocate_schemas(collection, &paths, &schema_copies));
        if let Err(error) = applied {
            Self::roll_back(
                collection,
                &mut active,
                (lsn, generation),
                (&previous, previous_schemas.as_deref(), &adopted),
            )?;
            return Err(error);
        }
        let stable = GenerationRecord::log_stable(generation, active.position());
        SegmentCheckpoint::publish_current(collection, &stable)?;
        SegmentRecovery::remove_unreferenced(&paths, active.sequence())?;
        failpoint("after-compaction-remove")?;
        Ok(CompactionReport {
            segments_compacted: self.candidates.len(),
            documents_copied: rows.len(),
            bytes_before: self.bytes_before,
            bytes_after: SegmentStore::file_length(&adopted)?,
        })
    }

    /// Cancel a compaction whose record is durable but whose adoption
    /// failed in this process: put every location back, cancel the record
    /// with an abort record, and drop the copy. Nothing is left for
    /// recovery.
    fn roll_back(
        collection: &NativeCollection,
        active: &mut LogSegment,
        (lsn, generation): (u64, u64),
        (previous, previous_schemas, adopted): (
            &[SegmentLocation],
            Option<&[u8]>,
            &std::path::Path,
        ),
    ) -> Result<(), NativeStorageError> {
        SegmentPlan::append_locations(&collection.path, previous)?;
        SegmentSchemas::restore(&SegmentPaths::of(&collection.path), previous_schemas)?;
        let manifest =
            serde_json::to_vec(&AbortManifest::new(lsn)).map_err(|error| json_error(&error))?;
        let record = LogRecord {
            lsn: active.next_lsn(),
            kind: LogRecordKind::Abort,
            generation,
            manifest,
            blobs: Vec::new(),
        };
        let encoded = active.encode(&record)?;
        active.append(&encoded, 1)?;
        active.full_flush()?;
        let stable = GenerationRecord::log_stable(generation, active.position());
        SegmentCheckpoint::publish_current(collection, &stable)?;
        Self::remove_quietly(adopted);
        Ok(())
    }

    /// Undo a compaction whose record never became durable: cut the active
    /// segment back, publish the stable generation again and drop the copy.
    fn cancel(
        collection: &NativeCollection,
        active: &mut LogSegment,
        (before, generation): ((u64, u64, u64), u64),
        adopted: &std::path::Path,
    ) -> Result<(), NativeStorageError> {
        active.rewind(before)?;
        let stable = GenerationRecord::log_stable(generation, active.position());
        SegmentCheckpoint::publish_current(collection, &stable)?;
        Self::remove_quietly(adopted);
        Ok(())
    }

    /// Remove a copy nothing refers to. A copy that cannot be removed now is
    /// unreferenced, so the next recovery or compaction removes it.
    fn remove_quietly(path: &std::path::Path) {
        let _ = fylo_vfs::remove_file(path);
    }

    fn choose(
        paths: &SegmentPaths,
        (store, schemas): (&SegmentStore, &SegmentSchemas),
        active: u64,
    ) -> Result<Option<Self>, NativeStorageError> {
        let rows = store.table().rows();
        let mut live: BTreeMap<u64, BTreeMap<u64, u32>> = BTreeMap::new();
        let records = rows
            .iter()
            .map(|row| (row.segment, row.offset, row.length))
            .chain(
                schemas
                    .entries()
                    .iter()
                    .map(|entry| (entry.segment, entry.offset, entry.length)),
            );
        for (segment, offset, length) in records {
            live.entry(segment).or_default().insert(offset, length);
        }
        let mut candidates = Vec::new();
        let mut bytes_before = 0;
        for number in paths.segment_numbers()? {
            if number == active {
                continue;
            }
            let size = SegmentStore::file_length(&paths.segment(number))?;
            let live_bytes: u64 = live.get(&number).map_or(0, |records| {
                records.values().map(|length| u64::from(*length)).sum()
            });
            let dead = size.saturating_sub(live_bytes + LogSegmentHeader::ENCODED_BYTES as u64);
            if dead > 0 || size < Self::SMALL_SEGMENT_BYTES {
                candidates.push((number, dead));
                bytes_before += size;
            }
        }
        let worthwhile = candidates.len() > 1 || candidates.iter().any(|(_, dead)| *dead > 0);
        if !worthwhile {
            return Ok(None);
        }
        let chosen: BTreeSet<u64> = candidates.iter().map(|(number, _)| *number).collect();
        let rows = rows
            .into_iter()
            .filter(|row| chosen.contains(&row.segment))
            .cloned()
            .collect();
        let schemas = schemas
            .entries()
            .iter()
            .filter(|entry| chosen.contains(&entry.segment))
            .cloned()
            .collect();
        Ok(Some(Self {
            candidates: chosen.into_iter().collect(),
            rows,
            schemas,
            bytes_before,
        }))
    }

    /// One live entry as a record of its own, keeping the sequence number
    /// and generation of the commit it came from.
    fn copied_frame(
        row: &SegmentLocation,
        record: &LogRecord,
    ) -> Result<Vec<u8>, NativeStorageError> {
        let document = SegmentDocument::from_record(row, record)?;
        let mut entry = document.entry;
        if let Some(body) = entry.body.as_mut() {
            body.offset = 0;
        }
        let manifest = SegmentCommitManifest {
            format: SegmentCommitManifest::FORMAT.into(),
            operation: "compaction".into(),
            entries: vec![entry],
            index_removed: Vec::new(),
            index_added: Vec::new(),
        };
        let copied = LogRecord {
            lsn: record.lsn,
            kind: LogRecordKind::Commit,
            generation: record.generation,
            manifest: serde_json::to_vec(&manifest).map_err(|error| json_error(&error))?,
            blobs: document.body,
        };
        LogSegment::frame(LogSegmentKind::CompactedDocuments, &copied)
    }

    /// A schema record as it is, framed for the compacted segment.
    fn copied_schema(
        reader: &SegmentReader,
        entry: &SchemaVersionEntry,
    ) -> Result<Vec<u8>, NativeStorageError> {
        let frame = reader.span(entry.segment, entry.offset, entry.length as usize)?;
        let record = SegmentReader::decode(&frame)?;
        if record.kind != LogRecordKind::Schema || record.lsn != entry.lsn {
            return Err(SegmentSchemas::corrupt(&format!(
                "schema version {} is not at its recorded location",
                entry.ordinal
            )));
        }
        LogSegment::frame(LogSegmentKind::CompactedDocuments, &record)
    }

    /// Point the schemas snapshot at the copies the adopted segment holds.
    fn relocate_schemas(
        collection: &NativeCollection,
        paths: &SegmentPaths,
        copies: &SchemaCopies,
    ) -> Result<(), NativeStorageError> {
        if copies.is_empty() {
            return Ok(());
        }
        SegmentSchemas::load(paths, collection.schema_head())?
            .relocated(copies)
            .write_if_changed(paths)
            .map(|_| ())
    }

    fn append_record(
        active: &mut LogSegment,
        (segment, sha256): (u64, String),
        replaces: &[u64],
        generation: u64,
    ) -> Result<(), NativeStorageError> {
        let manifest = CompactionManifest {
            format: CompactionManifest::FORMAT.into(),
            segment,
            sha256,
            replaces: replaces.to_vec(),
        };
        let record = LogRecord {
            lsn: active.next_lsn(),
            kind: LogRecordKind::Compaction,
            generation,
            manifest: serde_json::to_vec(&manifest).map_err(|error| json_error(&error))?,
            blobs: Vec::new(),
        };
        let encoded = active.encode(&record)?;
        active.append(&encoded, 1)?;
        active.full_flush()
    }

    /// Remove copies an interrupted compaction left. The compaction lock is
    /// held, so no other compactor is writing one.
    fn remove_temporaries(paths: &SegmentPaths) -> Result<(), NativeStorageError> {
        for entry in fylo_vfs::read_dir(paths.directory()).map_err(NativeStorageError::io)? {
            let entry = entry.map_err(NativeStorageError::io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(Self::TEMPORARY_PREFIX) && name.ends_with(Self::TEMPORARY_SUFFIX) {
                fylo_vfs::remove_file(entry.path()).map_err(NativeStorageError::io)?;
            }
        }
        Ok(())
    }

    pub(crate) fn require_segments(
        collection: &NativeCollection,
    ) -> Result<(), NativeStorageError> {
        if collection.uses_segments() {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "compaction applies only to a collection with the segment layout",
        ))
    }
}
