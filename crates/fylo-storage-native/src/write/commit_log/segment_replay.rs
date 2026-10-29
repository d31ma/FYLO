use std::collections::BTreeSet;

use fylo_format::{LogRecordKind, LogScan, LogSegmentHeader, LogSegmentKind};

use super::compaction_manifest::CompactionManifest;
use super::segment_rebuild::SegmentRebuild;
use crate::segments::{
    IndexDelta, LocationTable, RecordPosition, SegmentCommitManifest, SegmentMedium, SegmentReader,
};
use crate::write::NativeWriteRoot;
use crate::{NativeStorageError, NativeStorageErrorCode};

/// The prefix index's second source of changes, for a medium that keeps no
/// index log: the records, replayed as the location table replays them.
impl IndexDelta {
    /// The index log lines the commits from `from` up to `to` make, in
    /// record order, without those an abort record in the range cancelled.
    /// A compaction moves documents without changing their keys, so it
    /// makes none.
    pub(crate) fn replay(
        medium: &dyn SegmentMedium,
        (from, to): (RecordPosition, RecordPosition),
    ) -> Result<Vec<u8>, NativeStorageError> {
        let present = medium.segment_numbers()?;
        let (scans, _) = LocationTable::replayed_scans(medium, &present, (from, to), false)?;
        let aborted = SegmentRebuild::aborted(scans.iter().map(|(_, scan)| scan))?;
        let mut lines = Vec::new();
        for (_, scan) in &scans {
            for (_, record) in &scan.records {
                if record.kind != LogRecordKind::Commit || aborted.contains(&record.lsn) {
                    continue;
                }
                let manifest = SegmentCommitManifest::decode(&record.manifest)?;
                let removed: BTreeSet<String> = manifest.index_removed.into_iter().collect();
                let added: BTreeSet<String> = manifest.index_added.into_iter().collect();
                lines.extend(NativeWriteRoot::index_lines(&removed, &added));
            }
        }
        Ok(lines)
    }
}

/// The location table's second source of rows: the records themselves, for
/// a medium that keeps no location log (`docs/fxdb-segment-backend.md`,
/// "Location deltas without `locations.wal`").
impl LocationTable {
    /// Lay over the table the rows that the records from `from` up to `to`
    /// derive, as a rebuild derives them: each commit's entries, and for a
    /// compaction the copies whose sequence number still wins. A record
    /// that an abort record in the range cancelled derives nothing, where
    /// the location log holds rows that put back what it replaced.
    ///
    /// Both positions are record boundaries, such as a checkpoint record's
    /// start and a published slot's end, so an abort lies in the same range
    /// as the commit it cancels. The segments between them are read in
    /// number order. A compacted segment among them is skipped: it holds
    /// copies, which its compaction record brings in.
    ///
    /// A compaction whose copy no longer exists brings in nothing: a copy
    /// is removed only once no location refers to it, so every document it
    /// held is written again by a later record in the range.
    pub(crate) fn replay(
        &mut self,
        medium: &dyn SegmentMedium,
        range: (RecordPosition, RecordPosition),
    ) -> Result<(), NativeStorageError> {
        self.replay_reading(medium, range, false)
    }

    /// [`Self::replay`], stopping at the first record that cannot be read
    /// instead of failing, and marking the table damaged there: what a
    /// salvage or a verify of a medium without a location log can know
    /// without the rows past the damage. A segment whose header cannot be
    /// read is damaged at its start.
    pub(crate) fn replay_salvaging(
        &mut self,
        medium: &dyn SegmentMedium,
        range: (RecordPosition, RecordPosition),
    ) -> Result<(), NativeStorageError> {
        self.replay_reading(medium, range, true)
    }

    fn replay_reading(
        &mut self,
        medium: &dyn SegmentMedium,
        (from, to): (RecordPosition, RecordPosition),
        salvaging: bool,
    ) -> Result<(), NativeStorageError> {
        let present: Vec<u64> = medium.segment_numbers()?;
        let (scans, damage) = Self::replayed_scans(medium, &present, (from, to), salvaging)?;
        if let Some(damage) = damage {
            self.mark_damaged(damage);
        }
        let aborted = SegmentRebuild::aborted(scans.iter().map(|(_, scan)| scan))?;
        for (number, scan) in &scans {
            let lengths = SegmentRebuild::frame_lengths(scan);
            for ((offset, record), length) in scan.records.iter().zip(lengths) {
                if aborted.contains(&record.lsn) {
                    continue;
                }
                let rows = match record.kind {
                    LogRecordKind::Commit => {
                        SegmentRebuild::rows_of(*number, (*offset, length), record)?
                    }
                    LogRecordKind::Compaction => {
                        let manifest = CompactionManifest::decode(&record.manifest)?;
                        if present.contains(&manifest.segment) {
                            CompactionManifest::adopted(medium, self, manifest.segment)?.0
                        } else {
                            Vec::new()
                        }
                    }
                    _ => Vec::new(),
                };
                for row in rows {
                    self.apply(row);
                }
            }
        }
        Ok(())
    }

    /// The records between two positions, as one scan per segment, of the
    /// segments `present`.
    ///
    /// The segment `from` names may be gone. A medium without a location
    /// log writes a location snapshot at every checkpoint, sealing
    /// included, so a snapshot position in a segment sealed since is that
    /// segment's end: nothing past it remains to replay, and a sealed
    /// segment may be compacted away. The segment `to` names must
    /// exist; when it does not, the generation moved under the reader.
    #[allow(clippy::type_complexity)]
    fn replayed_scans(
        medium: &dyn SegmentMedium,
        present: &[u64],
        (from, to): (RecordPosition, RecordPosition),
        salvaging: bool,
    ) -> Result<(Vec<(u64, LogScan)>, Option<RecordPosition>), NativeStorageError> {
        if to <= from {
            return Ok((Vec::new(), None));
        }
        let mut numbers: Vec<u64> = present
            .iter()
            .copied()
            .filter(|number| (from.segment..=to.segment).contains(number))
            .collect();
        numbers.push(to.segment);
        numbers.sort_unstable();
        numbers.dedup();
        let mut scans = Vec::with_capacity(numbers.len());
        for number in numbers {
            let start = (number == from.segment).then_some(from.offset);
            let end = (number == to.segment).then_some(to.offset);
            let (scan, damage) = Self::replayed_span(medium, number, (start, end), salvaging)?;
            if let Some(scan) = scan {
                scans.push((number, scan));
            }
            if let Some(offset) = damage {
                return Ok((
                    scans,
                    Some(RecordPosition {
                        segment: number,
                        offset,
                    }),
                ));
            }
        }
        Ok((scans, None))
    }

    /// The records of segment `number` from `start` to `end`, by default
    /// from its first record to its end, or `None` for a compacted segment.
    /// Every record in the span must decode: it was committed. When
    /// `salvaging`, the first that does not ends the span instead, and its
    /// offset comes back with the records before it.
    fn replayed_span(
        medium: &dyn SegmentMedium,
        number: u64,
        (start, end): (Option<u64>, Option<u64>),
        salvaging: bool,
    ) -> Result<(Option<LogScan>, Option<u64>), NativeStorageError> {
        let handle = medium.read_handle(number)?;
        let mut header = [0_u8; LogSegmentHeader::ENCODED_BYTES];
        handle.read_at(&mut header, 0)?;
        let header = match LogSegmentHeader::decode(&header) {
            Ok(header) => header,
            Err(_) if salvaging => return Ok((None, Some(0))),
            Err(error) => return Err(Self::damaged(number, 0, &error.to_string())),
        };
        if header.kind != LogSegmentKind::Documents {
            return Ok((None, None));
        }
        let start = start.unwrap_or(LogSegmentHeader::ENCODED_BYTES as u64);
        let end = match end {
            Some(end) => end,
            None => handle.length()?,
        };
        let size = end
            .checked_sub(start)
            .and_then(|size| usize::try_from(size).ok())
            .ok_or_else(|| Self::damaged(number, start, "the span to replay is invalid"))?;
        let mut bytes = vec![0_u8; size];
        handle.read_at(&mut bytes, start)?;
        let mut records = Vec::new();
        let mut cursor = 0;
        let bodies = medium.frame_bodies();
        while cursor < bytes.len() {
            let offset = start + cursor as u64;
            let (record, length) = match header.kind.unframe_with(&bytes[cursor..], &*bodies) {
                Ok(framed) => framed,
                Err(error) if salvaging => {
                    let scan = LogScan {
                        header,
                        records,
                        valid_length: offset,
                        stopped: Some(error),
                    };
                    return Ok((Some(scan), Some(offset)));
                }
                Err(error) if error.is_unreadable() => {
                    return Err(SegmentReader::frame_error(
                        error,
                        &format!("segment {number} at byte {offset}"),
                    ));
                }
                Err(error) => return Err(Self::damaged(number, offset, &error.to_string())),
            };
            records.push((offset, record));
            cursor += length;
        }
        let scan = LogScan {
            header,
            records,
            valid_length: end,
            stopped: None,
        };
        Ok((Some(scan), None))
    }

    fn damaged(number: u64, offset: u64, detail: &str) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::CorruptDocument,
            format!("segment {number} cannot be replayed at byte {offset}: {detail}"),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use fylo_format::{LogRecordKind, LogScan};
    use serde_json::json;

    use super::super::super::{NativeWriteRoot, unique_name};
    use super::super::logged_plan::LoggedPlan;
    use super::super::placed_record::PlacedRecord;
    use super::super::segment_checkpoint::SegmentCheckpoint;
    use super::super::segment_plan::SegmentPlan;
    use crate::segments::{
        LocationState, LocationTable, RecordPosition, SegmentCheckpointManifest, SegmentLocation,
        SegmentMedium, SegmentStore, TableReach,
    };
    use crate::{
        CollectionKind, CollectionLayout, NativeCollection, NativeRoot, NativeStorageError,
        NativeStorageErrorCode, PutDocumentOptions, RootConfig,
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn create() -> Self {
            let path = std::env::temp_dir().join(unique_name("fylo-segment-replay"));
            std::fs::create_dir(&path).unwrap();
            let fixture = Self(std::fs::canonicalize(path).unwrap());
            fixture
                .writer()
                .create_collection("items", CollectionKind::Document, None)
                .unwrap();
            fixture
        }

        fn writer(&self) -> NativeWriteRoot {
            NativeWriteRoot::open(&self.0)
                .unwrap()
                .with_config(RootConfig {
                    layout: CollectionLayout::Segments,
                    ..RootConfig::default()
                })
        }

        fn collection(&self) -> NativeCollection {
            NativeRoot::open(&self.0)
                .unwrap()
                .collection("items")
                .unwrap()
        }

        /// Seal the active segment and open the next, as a checkpoint does
        /// once the active segment is full.
        fn seal(&self) {
            let writer = self.writer();
            let collection = self.collection();
            let _lock = writer.lock_collection(&collection).unwrap();
            let stored = collection.stored_generation().unwrap();
            let active = NativeWriteRoot::open_active_segment(&collection, &stored).unwrap();
            SegmentCheckpoint::seal(&collection, active, stored.generation).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn identifier(number: usize) -> String {
        format!("4VRNF5{:05}", 30_000 + number)
    }

    fn put(writer: &NativeWriteRoot, number: usize) {
        let body = json!({ "group": number % 5, "name": format!("user{number}") }).to_string();
        writer
            .put_document(
                "items",
                &identifier(number),
                body.as_bytes(),
                PutDocumentOptions::default(),
            )
            .unwrap();
    }

    /// Puts, patches, soft deletes and a restore over `numbers`.
    fn mutate(writer: &NativeWriteRoot, numbers: std::ops::Range<usize>) {
        for number in numbers.clone() {
            put(writer, number);
        }
        for number in numbers.clone().step_by(2) {
            let patch = json!({ "patched": number }).to_string();
            writer
                .patch_document("items", &identifier(number), patch.as_bytes(), None)
                .unwrap();
        }
        for number in numbers.clone().step_by(3) {
            writer
                .delete_document("items", &identifier(number), None)
                .unwrap();
        }
        writer
            .restore_document("items", &identifier(numbers.start), None)
            .unwrap();
    }

    /// The table the location log gives, and where the records end, at the
    /// published slot.
    fn published(medium: &dyn SegmentMedium) -> (LocationTable, RecordPosition) {
        let slot = medium.require_slot().unwrap();
        let table = LocationTable::load(medium, TableReach::Log(slot.locations_length)).unwrap();
        let end = RecordPosition {
            segment: slot.segment,
            offset: slot.length,
        };
        (table, end)
    }

    /// The table the location log gave when segment `number` opened, and
    /// where its opening checkpoint record starts.
    fn opening(medium: &dyn SegmentMedium, number: u64) -> (LocationTable, RecordPosition) {
        let bytes = medium.require_segment(number).unwrap();
        let scan = LogScan::segment_with(&bytes, &*medium.frame_bodies()).unwrap();
        let (offset, record) = &scan.records[0];
        let manifest: SegmentCheckpointManifest = serde_json::from_slice(&record.manifest).unwrap();
        let table =
            LocationTable::load(medium, TableReach::Log(manifest.locations_wal_length)).unwrap();
        let start = RecordPosition {
            segment: number,
            offset: *offset,
        };
        (table, start)
    }

    fn rows(table: &LocationTable) -> Vec<SegmentLocation> {
        table.rows().into_iter().cloned().collect()
    }

    #[test]
    fn replaying_records_across_segments_gives_the_logged_table() {
        let fixture = Fixture::create();
        let writer = fixture.writer();
        mutate(&writer, 0..30);
        fixture.seal();
        mutate(&writer, 30..50);
        fixture.seal();
        mutate(&writer, 50..60);
        let medium = fixture.collection().segment_medium().unwrap();
        assert_eq!(medium.segment_numbers().unwrap(), vec![1, 2, 3]);
        let (logged, end) = published(&*medium);
        let (mut replayed, start) = opening(&*medium, 1);
        assert!(rows(&replayed).is_empty());
        replayed.replay(&*medium, (start, end)).unwrap();
        assert_eq!(rows(&replayed), rows(&logged));
        assert_eq!(
            replayed.encode_snapshot().unwrap(),
            logged.encode_snapshot().unwrap()
        );
        let (mut tail, start) = opening(&*medium, 3);
        tail.replay(&*medium, (start, end)).unwrap();
        assert_eq!(rows(&tail), rows(&logged));
    }

    #[test]
    fn replaying_a_compaction_adopts_the_copies_the_log_adopted() {
        let fixture = Fixture::create();
        let writer = fixture.writer();
        mutate(&writer, 0..40);
        writer.compact_collection("items").unwrap();
        mutate(&writer, 40..50);
        for number in (1..20).filter(|number| number % 3 != 0) {
            let patch = json!({ "again": number }).to_string();
            writer
                .patch_document("items", &identifier(number), patch.as_bytes(), None)
                .unwrap();
        }
        fixture.seal();
        mutate(&writer, 50..55);
        let medium = fixture.collection().segment_medium().unwrap();
        assert_eq!(
            medium.segment_numbers().unwrap(),
            vec![2, 3, 4],
            "segment 3 is the compacted copy"
        );
        let (logged, end) = published(&*medium);
        let (mut replayed, start) = opening(&*medium, 2);
        assert!(
            rows(&replayed).iter().all(|row| row.segment == 1),
            "segment 2 opened before the compaction"
        );
        replayed.replay(&*medium, (start, end)).unwrap();
        assert!(rows(&replayed).iter().any(|row| row.segment == 3));
        assert_eq!(rows(&replayed), rows(&logged));
        assert_eq!(
            replayed.encode_snapshot().unwrap(),
            logged.encode_snapshot().unwrap()
        );
    }

    /// A plan whose apply fails once its location rows and index delta are
    /// appended, so the writer undoes it and cancels it with an abort record.
    struct FailingApply(SegmentPlan);

    impl LoggedPlan for FailingApply {
        fn touched(&self) -> &BTreeSet<PathBuf> {
            self.0.touched()
        }

        fn manifest(&self) -> Result<Vec<u8>, NativeStorageError> {
            self.0.manifest()
        }

        fn blobs(&self) -> &[u8] {
            self.0.blobs()
        }

        fn apply(
            &self,
            root: &NativeRoot,
            collection: &NativeCollection,
            placed: &PlacedRecord,
        ) -> Result<(), (NativeStorageError, bool)> {
            self.0.apply(root, collection, placed)?;
            Err((
                NativeStorageError::new(NativeStorageErrorCode::Io, "apply failed"),
                true,
            ))
        }

        fn undo(
            &self,
            root: &NativeRoot,
            collection: &NativeCollection,
            index_applied: bool,
        ) -> Result<(), NativeStorageError> {
            self.0.undo(root, collection, index_applied)
        }
    }

    /// Commit a rewrite of document `number` and a new document `added`,
    /// and fail its apply.
    fn commit_and_abort(fixture: &Fixture, (number, added): (usize, usize)) {
        let writer = fixture.writer();
        let collection = fixture.collection();
        let failed = writer.commit_logged(&collection, || {
            let store = SegmentStore::open_for_writer(collection.segment_medium()?)?;
            let document = store.document(&identifier(number), LocationState::Live)?;
            let mut plan = SegmentPlan::new("rewrite");
            let mut copy = document.entry.clone();
            copy.id = identifier(added);
            plan.push(
                &collection.path,
                (document.entry.clone(), Some(&document.body)),
                Some(document.location.clone()),
            )?;
            plan.push(&collection.path, (copy, Some(&document.body)), None)?;
            Ok((Some(FailingApply(plan)), ()))
        });
        assert_eq!(failed.unwrap_err().code(), NativeStorageErrorCode::Io);
    }

    #[test]
    fn replaying_skips_a_commit_an_abort_cancelled() {
        let fixture = Fixture::create();
        let writer = fixture.writer();
        mutate(&writer, 0..10);
        commit_and_abort(&fixture, (1, 900));
        mutate(&writer, 10..15);
        let medium = fixture.collection().segment_medium().unwrap();
        let bytes = medium.require_segment(1).unwrap();
        let scan = LogScan::segment_with(&bytes, &*medium.frame_bodies()).unwrap();
        assert!(
            scan.records
                .iter()
                .any(|(_, record)| record.kind == LogRecordKind::Abort)
        );
        let (logged, end) = published(&*medium);
        assert!(logged.get(&identifier(900)).is_none());
        let (mut replayed, start) = opening(&*medium, 1);
        replayed.replay(&*medium, (start, end)).unwrap();
        assert!(replayed.get(&identifier(900)).is_none());
        assert_eq!(rows(&replayed), rows(&logged));
        assert_eq!(
            replayed.encode_snapshot().unwrap(),
            logged.encode_snapshot().unwrap()
        );
    }

    #[test]
    fn an_empty_range_replays_nothing() {
        let fixture = Fixture::create();
        mutate(&fixture.writer(), 0..5);
        let medium = fixture.collection().segment_medium().unwrap();
        let (logged, end) = published(&*medium);
        let mut replayed = logged.clone();
        replayed.replay(&*medium, (end, end)).unwrap();
        assert_eq!(rows(&replayed), rows(&logged));
    }
}
