use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use fylo_format::{LogBodyCodec, LogRecord, LogRecordKind, LogSegmentHeader, LogSegmentKind};

use super::super::{GenerationRecord, failpoint, json_error};
use super::abort_manifest::AbortManifest;
use super::compaction_manifest::CompactionManifest;
use super::compaction_report::CompactionReport;
use super::log_segment::LogSegment;
use super::segment_checkpoint::SegmentCheckpoint;
use super::segment_plan::SegmentPlan;
use super::segment_recovery::{SchemaCopies, SegmentRecovery};
use super::segment_schemas::{SchemaVersionEntry, SegmentSchemas};
use crate::segments::{
    SegmentCommitManifest, SegmentDocument, SegmentHandle, SegmentLocation, SegmentMedium,
    SegmentReader, SegmentStore,
};
use crate::{
    Durability, GenerationState, NativeCollection, NativeStorageError, NativeStorageErrorCode,
};

/// Reclaims the space dead entries take in sealed segments (ADR 0015
/// section 7).
///
/// The copy runs without the collection lock, because sealed segments never
/// change. It is started under the lock, while the segments are chosen, so
/// writing it takes no claim on the collection: in a `.fxdb` database the
/// collection's writers carry on while it runs. Each live entry keeps its
/// sequence number, so the copy and the original are interchangeable, and
/// the compaction commits by appending one compaction record to the active
/// segment under the lock.
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
    /// This process's compacted copy, started when the segments were
    /// chosen.
    copy: Arc<dyn SegmentHandle>,
}

/// What a test runs while a compaction's copy is in progress.
#[cfg(test)]
pub(crate) type CopiedHook = Arc<dyn Fn(&std::path::Path) + Send + Sync>;

/// The segments one compaction replaces, with the live entries to copy.
struct Choice {
    candidates: Vec<u64>,
    rows: Vec<SegmentLocation>,
    schemas: Vec<SchemaVersionEntry>,
    bytes_before: u64,
}

impl SegmentCompactor {
    const SMALL_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;
    /// What a copied entry may add to its record's bytes: its own manifest
    /// header, which an entry shares with others in the original.
    const COPY_OVERHEAD_BYTES: u64 = 256;

    /// Choose what to compact and start the copy. Runs under the collection
    /// lock. The active segment is sealed first when it holds documents, so
    /// everything written so far can be compacted.
    ///
    /// With `only`, the compaction moves that one segment, live or not, as
    /// `fylo compact --shrink` does to bring the last extents of a `.fxdb`
    /// file nearer its front: the active segment is sealed only when it is
    /// that one, and no dictionary is trained. A segment no location names
    /// is removed without a copy.
    pub(crate) fn prepare(
        collection: &NativeCollection,
        stored: &GenerationState,
        active: LogSegment,
        only: Option<u64>,
    ) -> Result<Option<Self>, NativeStorageError> {
        let medium = collection.segment_medium()?;
        // A newer dictionary is made current first, so the copy, the
        // checkpoint that seals the active segment and the next segment
        // all use it, and the one it replaces can be released at adoption:
        // the active segment is sealed for it even when it holds no
        // documents, so no record left in it names the old one.
        let retrained = only.is_none() && medium.retrain_dictionary()?;
        let store = SegmentStore::open_for_writer(Arc::clone(&medium))?;
        let active_number = active.sequence();
        let holds_documents = store
            .table()
            .rows()
            .iter()
            .any(|row| row.segment == active_number);
        let seal = match only {
            Some(number) => number == active_number,
            None => holds_documents || retrained,
        };
        let active = if seal {
            SegmentCheckpoint::seal(collection, active, stored.generation)?
        } else {
            active
        };
        medium.remove_compactions()?;
        SegmentRecovery::remove_unreferenced(&*medium, active.sequence())?;
        let store = SegmentStore::open_for_writer(Arc::clone(&medium))?;
        let schemas = SegmentSchemas::load(&*medium, collection.schema_head())?;
        let chosen = match only {
            Some(number) => {
                Self::choose_only(&*medium, (&store, &schemas), active.sequence(), number)?
            }
            None => Self::choose(&*medium, (&store, &schemas), active.sequence())?,
        };
        let Some(choice) = chosen else {
            return Ok(None);
        };
        Ok(Some(Self {
            candidates: choice.candidates,
            rows: choice.rows,
            schemas: choice.schemas,
            bytes_before: choice.bytes_before,
            copy: medium.create_compaction()?,
        }))
    }

    /// Copy every live entry of the chosen segments, in identifier order,
    /// into this process's compacted copy. Runs without the lock.
    pub(crate) fn copy(&self, collection: &NativeCollection) -> Result<(), NativeStorageError> {
        let medium = collection.segment_medium()?;
        let reader = SegmentReader::new(Arc::clone(&medium));
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
        let copy = &self.copy;
        let encoded = header.encode();
        copy.write_at(&encoded, 0)?;
        let mut length = encoded.len() as u64;
        for entry in &self.schemas {
            let frame = Self::copied_schema(&reader, entry)?;
            copy.write_at(&frame, length)?;
            length += frame.len() as u64;
        }
        let mut records: BTreeMap<(u64, u64), LogRecord> = BTreeMap::new();
        for row in &self.rows {
            let key = (row.segment, row.offset);
            if let Entry::Vacant(slot) = records.entry(key) {
                slot.insert(reader.record(row)?);
            }
            let frame = Self::copied_frame(row, &records[&key], &*medium.frame_bodies())?;
            copy.write_at(&frame, length)?;
            length += frame.len() as u64;
        }
        copy.sync(Durability::PowerLoss)?;
        #[cfg(test)]
        Self::copied_hook(&collection.path);
        failpoint("after-compaction-copy")?;
        Ok(())
    }

    /// Run the hook a test set with [`Self::set_copied_hook`], if any.
    #[cfg(test)]
    fn copied_hook(collection: &std::path::Path) {
        let hook = Self::hook().clone();
        if let Some(hook) = hook {
            hook(collection);
        }
    }

    /// Run `hook` with the collection's path once each compaction's copy
    /// is written and flushed, before the collection is locked to adopt
    /// it, so a test can act while a copy is in progress.
    #[cfg(test)]
    pub(crate) fn set_copied_hook(hook: Option<CopiedHook>) {
        *Self::hook() = hook;
    }

    #[cfg(test)]
    fn hook() -> std::sync::MutexGuard<'static, Option<CopiedHook>> {
        static HOOK: std::sync::Mutex<Option<CopiedHook>> = std::sync::Mutex::new(None);
        HOOK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Adopt the copy. Runs under the collection lock: the compaction record
    /// appended to the active segment is the commit point.
    pub(crate) fn adopt(
        &self,
        collection: &NativeCollection,
        (stored, mut active): (&GenerationState, LogSegment),
    ) -> Result<CompactionReport, NativeStorageError> {
        let medium = collection.segment_medium()?;
        let sha256 = crate::sha256_hex(&medium.read_compaction()?);
        let number = medium.adopt_compaction()?;
        if let Err(error) = failpoint("after-compaction-sync") {
            Self::remove_quietly(&*medium, number);
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
            Self::cancel(collection, &mut active, (before, generation), number)?;
            return Err(error);
        }
        let store = SegmentStore::open_for_writer(Arc::clone(&medium))?;
        let (rows, schema_copies) = CompactionManifest::adopted(&*medium, store.table(), number)?;
        let previous: Vec<SegmentLocation> = rows
            .iter()
            .filter_map(|row| store.table().get(&row.id).cloned())
            .collect();
        let previous_schemas = SegmentSchemas::snapshot_bytes(&*medium)?;
        let applied = failpoint("after-compaction-record")
            .and_then(|()| SegmentPlan::append_locations(&*medium, &rows))
            .and_then(|()| Self::relocate_schemas(collection, &*medium, &schema_copies));
        if let Err(error) = applied {
            Self::roll_back(
                collection,
                &mut active,
                (lsn, generation),
                (&previous, previous_schemas.as_deref(), number),
            )?;
            return Err(error);
        }
        let stable = GenerationRecord::log_stable(generation, active.position());
        SegmentCheckpoint::publish_current(collection, &stable)?;
        SegmentRecovery::remove_unreferenced(&*medium, active.sequence())?;
        failpoint("after-compaction-remove")?;
        medium.release_unused_dictionaries()?;
        Ok(CompactionReport {
            segments_compacted: self.candidates.len(),
            documents_copied: rows.len(),
            bytes_before: self.bytes_before,
            bytes_after: medium.segment_length(number)?,
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
        (previous, previous_schemas, adopted): (&[SegmentLocation], Option<&[u8]>, u64),
    ) -> Result<(), NativeStorageError> {
        let medium = collection.segment_medium()?;
        SegmentPlan::append_locations(&*medium, previous)?;
        SegmentSchemas::restore(&*medium, previous_schemas)?;
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
        Self::remove_quietly(&*medium, adopted);
        Ok(())
    }

    /// Undo a compaction whose record never became durable: cut the active
    /// segment back, publish the stable generation again and drop the copy.
    fn cancel(
        collection: &NativeCollection,
        active: &mut LogSegment,
        (before, generation): ((u64, u64, u64), u64),
        adopted: u64,
    ) -> Result<(), NativeStorageError> {
        active.rewind(before)?;
        let stable = GenerationRecord::log_stable(generation, active.position());
        SegmentCheckpoint::publish_current(collection, &stable)?;
        Self::remove_quietly(&*collection.segment_medium()?, adopted);
        Ok(())
    }

    /// Remove a copy nothing refers to. A copy that cannot be removed now is
    /// unreferenced, so the next recovery or compaction removes it.
    fn remove_quietly(medium: &dyn SegmentMedium, adopted: u64) {
        let _ = medium.remove_segment(adopted);
    }

    /// The choice that moves segment `number` alone, when it is a sealed
    /// segment whose live entries fit in one copy.
    fn choose_only(
        medium: &dyn SegmentMedium,
        (store, schemas): (&SegmentStore, &SegmentSchemas),
        active: u64,
        number: u64,
    ) -> Result<Option<Choice>, NativeStorageError> {
        if number == active || !medium.segment_numbers()?.contains(&number) {
            return Ok(None);
        }
        let rows: Vec<SegmentLocation> = store
            .table()
            .rows()
            .into_iter()
            .filter(|row| row.segment == number)
            .cloned()
            .collect();
        let schemas: Vec<SchemaVersionEntry> = schemas
            .entries()
            .iter()
            .filter(|entry| entry.segment == number)
            .cloned()
            .collect();
        let copy_bytes: u64 = rows
            .iter()
            .map(|row| row.length)
            .chain(schemas.iter().map(|entry| entry.length))
            .map(|length| u64::from(length) + Self::COPY_OVERHEAD_BYTES)
            .sum();
        let room = medium
            .max_segment_bytes()
            .saturating_sub(LogSegmentHeader::ENCODED_BYTES as u64);
        if copy_bytes > room {
            return Ok(None);
        }
        Ok(Some(Choice {
            candidates: vec![number],
            rows,
            schemas,
            bytes_before: medium.segment_length(number)?,
        }))
    }

    fn choose(
        medium: &dyn SegmentMedium,
        (store, schemas): (&SegmentStore, &SegmentSchemas),
        active: u64,
    ) -> Result<Option<Choice>, NativeStorageError> {
        let rows = store.table().rows();
        let mut live: BTreeMap<u64, BTreeMap<u64, u32>> = BTreeMap::new();
        let mut copied: BTreeMap<u64, u64> = BTreeMap::new();
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
            *copied.entry(segment).or_default() += u64::from(length) + Self::COPY_OVERHEAD_BYTES;
        }
        // The copy is one segment, so the chosen segments' live entries must
        // fit in one: a medium of small segments compacts a few at a time.
        let room = medium
            .max_segment_bytes()
            .saturating_sub(LogSegmentHeader::ENCODED_BYTES as u64);
        let mut copy_bytes = 0;
        let mut candidates = Vec::new();
        let mut bytes_before = 0;
        for number in medium.segment_numbers()? {
            if number == active {
                continue;
            }
            let size = medium.segment_length(number)?;
            let live_bytes: u64 = live.get(&number).map_or(0, |records| {
                records.values().map(|length| u64::from(*length)).sum()
            });
            let dead = size.saturating_sub(live_bytes + LogSegmentHeader::ENCODED_BYTES as u64);
            let copy = copied.get(&number).copied().unwrap_or(0);
            if (dead > 0 || size < Self::SMALL_SEGMENT_BYTES) && copy_bytes + copy <= room {
                candidates.push((number, dead));
                bytes_before += size;
                copy_bytes += copy;
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
        Ok(Some(Choice {
            candidates: chosen.into_iter().collect(),
            rows,
            schemas,
            bytes_before,
        }))
    }

    /// One live entry as a record of its own, keeping the sequence number
    /// and generation of the commit it came from. Its body is compressed
    /// afresh, so a `.fxdb` collection's LZ4 records come out as zstd.
    fn copied_frame(
        row: &SegmentLocation,
        record: &LogRecord,
        bodies: &dyn LogBodyCodec,
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
        LogSegment::frame(LogSegmentKind::CompactedDocuments, &copied, bodies)
    }

    /// A schema record as it is, framed for the compacted segment.
    fn copied_schema(
        reader: &SegmentReader,
        entry: &SchemaVersionEntry,
    ) -> Result<Vec<u8>, NativeStorageError> {
        let frame = reader.span(entry.segment, entry.offset, entry.length as usize)?;
        let bodies = reader.medium().frame_bodies();
        let record = SegmentReader::decode(&frame, &*bodies)?;
        if record.kind != LogRecordKind::Schema || record.lsn != entry.lsn {
            return Err(SegmentSchemas::corrupt(&format!(
                "schema version {} is not at its recorded location",
                entry.ordinal
            )));
        }
        LogSegment::frame(LogSegmentKind::CompactedDocuments, &record, &*bodies)
    }

    /// Point the schemas snapshot at the copies the adopted segment holds.
    fn relocate_schemas(
        collection: &NativeCollection,
        medium: &dyn SegmentMedium,
        copies: &SchemaCopies,
    ) -> Result<(), NativeStorageError> {
        if copies.is_empty() {
            return Ok(());
        }
        SegmentSchemas::load(medium, collection.schema_head())?
            .relocated(copies)
            .write_if_changed(medium)
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

    pub(crate) fn maintenance_unsupported() -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "this build does not carry segment maintenance",
        )
    }

    pub(crate) fn require_segments(
        collection: &NativeCollection,
    ) -> Result<(), NativeStorageError> {
        // Maintenance stays native: a browser build edits a `.fxdb`
        // database's segments but leaves compacting, exporting, verifying
        // and salvaging them to the native engine, so their code compiles
        // away there.
        if !NativeCollection::SEGMENTS_SUPPORTED {
            return Err(Self::maintenance_unsupported());
        }
        if collection.uses_segments() {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "compaction applies only to a collection with the segment layout",
        ))
    }
}
