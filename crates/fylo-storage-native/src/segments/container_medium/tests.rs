use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fylo_format::{
    ContainerExtentKind, ContainerGenerationSlot, LogRecord, LogScan, LogSegmentKind,
};
use serde_json::json;

use super::{ContainerMedium, LOCATIONS, SEGMENT};
use crate::segments::{
    LocationState, LocationTable, RecordPosition, SegmentMedium, SegmentStore, TableCache,
    TableReach,
};
use crate::{
    CollectionKind, CollectionLayout, ContainerFile, NativeCollection, NativeRoot,
    NativeStorageErrorCode, NativeWriteRoot, PutDocumentOptions, RootConfig,
};

mod database_compaction;
mod dictionaries;
mod encryption;
mod processes;
mod schemas;
mod stranded_writes;

struct Fixture(PathBuf);

impl Fixture {
    fn create(label: &str) -> Self {
        let path = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&path).unwrap();
        let fixture = Self(std::fs::canonicalize(path).unwrap());
        fixture
            .writer()
            .create_collection("items", CollectionKind::Document, None)
            .unwrap();
        fixture
    }

    fn writer(&self) -> NativeWriteRoot {
        writer_at(&self.0)
    }

    fn collection(&self) -> NativeCollection {
        NativeRoot::open(&self.0)
            .unwrap()
            .collection("items")
            .unwrap()
    }

    fn file(&self) -> PathBuf {
        self.collection().path.join(ContainerMedium::FILE_NAME)
    }

    fn medium(&self) -> Arc<dyn SegmentMedium> {
        self.collection().segment_medium().unwrap()
    }

    fn container(&self) -> ContainerMedium {
        ContainerMedium::of(&self.collection().path)
            .unwrap()
            .expect("the collection keeps its segments in a container")
    }

    /// Stop as a process would, losing what it held in memory: the extent
    /// map since its last checkpoint and every cached table.
    fn crash(&self) {
        let medium = self.medium();
        TableCache::shared().forget(&*medium);
        ContainerMedium::forget(&self.collection().path);
    }

    fn bodies(&self) -> BTreeMap<String, serde_json::Value> {
        let collection = self.collection();
        collection
            .document_ids()
            .unwrap()
            .into_iter()
            .map(|id| {
                let bytes = collection.read_document(&id).unwrap().bytes;
                (id, serde_json::from_slice(&bytes).unwrap())
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        ContainerMedium::forget(&self.0.join(".collections/items"));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl ContainerMedium {
    fn generation(&self) -> ContainerGenerationSlot {
        self.shared
            .file()
            .read_generation(self.shared.block)
            .unwrap()
            .unwrap()
    }

    fn extent_of(&self, number: u64) -> u64 {
        self.shared
            .extent(&mut self.shared.file(), number)
            .unwrap()
            .expect("the segment exists")
    }

    fn is_mapped(&self, extent: u64) -> bool {
        self.shared.file().allocator().is_mapped(extent)
    }

    fn holds(&self, extent: u64) -> Option<(ContainerExtentKind, u32, u64)> {
        self.shared.file().allocator().in_use(extent)
    }

    fn sealed(&self, extent: u64) -> Option<u64> {
        self.shared.file().allocator().sealed_live_bytes(extent)
    }

    /// The segment table, and the collection's segments as the map shows
    /// them, which it must match.
    fn tables(&self) -> (BTreeMap<u64, u64>, BTreeMap<u64, u64>) {
        let mut open = self.shared.file();
        let mapped = open
            .allocator()
            .owned(SEGMENT, self.shared.collection)
            .into_iter()
            .map(|(extent, number)| (number, extent))
            .collect();
        (open.table(self.shared.collection).clone(), mapped)
    }
}

/// A writer on the root at `path` that keeps new collections' segments in
/// a container.
fn writer_at(path: &Path) -> NativeWriteRoot {
    NativeWriteRoot::open(path)
        .unwrap()
        .with_config(RootConfig {
            layout: CollectionLayout::Segments,
            ..RootConfig::default()
        })
        .with_segment_container()
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 40_000 + number)
}

/// Put a document of about 4 KiB once compressed, so a 1 MiB extent fills
/// after a few hundred.
fn put(writer: &NativeWriteRoot, number: usize) {
    put_into(writer, "items", number).unwrap();
}

fn put_into(
    writer: &NativeWriteRoot,
    collection: &str,
    number: usize,
) -> Result<(), crate::NativeStorageError> {
    let body = json!({
        "name": format!("user{number}"),
        "padding": fylo_test_support::incompressible_text(u64::try_from(number).unwrap(), 5472),
    })
    .to_string();
    writer.put_document(
        collection,
        &identifier(number),
        body.as_bytes(),
        PutDocumentOptions::default(),
    )
}

fn patch(writer: &NativeWriteRoot, number: usize, round: usize) {
    let patch = json!({ "round": round }).to_string();
    writer
        .patch_document("items", &identifier(number), patch.as_bytes(), None)
        .unwrap();
}

#[test]
fn a_full_extent_rotates_with_a_map_checkpoint() {
    let fixture = Fixture::create("fxdb-medium-rotate");
    let writer = fixture.writer();
    let container = fixture.container();
    let first = container.segment_numbers().unwrap()[0];
    let first_extent = container.extent_of(first);
    assert!(
        container.is_mapped(first_extent),
        "the map is checkpointed once a segment's extent is taken"
    );
    for number in 0..300 {
        put(&writer, number);
    }
    let numbers = container.segment_numbers().unwrap();
    assert!(numbers.len() >= 2, "{numbers:?}");
    assert_eq!(numbers[0], first);
    let capacity = container.max_segment_bytes();
    for number in &numbers {
        let length = container.segment_length(*number).unwrap();
        assert!(length > 0 && length <= capacity, "{number}: {length}");
    }
    let active = container.generation();
    assert_eq!(active.active_sequence, *numbers.last().unwrap());
    assert!(
        numbers
            .iter()
            .all(|number| container.is_mapped(container.extent_of(*number))),
        "every segment a slot can name is in the durable map"
    );
    let checkpoint = container
        .shared
        .file()
        .read_checkpoint(container.shared.block)
        .unwrap()
        .expect("sealing writes a location snapshot");
    assert!(checkpoint.locations.is_some());
    let bodies = fixture.bodies();
    assert_eq!(bodies.len(), 300);
    assert_eq!(bodies[&identifier(7)]["name"], "user7");
}

#[test]
fn a_reopen_finds_the_active_extent_only_its_slot_names() {
    let fixture = Fixture::create("fxdb-medium-crash");
    let writer = fixture.writer();
    for number in 0..20 {
        put(&writer, number);
    }
    let container = fixture.container();
    let slot = container.generation();
    let before = fixture.bodies();
    drop(container);
    fixture.crash();

    // Stand in for a file whose map predates its active extent, as one
    // written before the map was checkpointed with each new segment: write
    // a map that shows the extent free.
    let mut file = ContainerFile::open_unchecked(&fixture.file()).unwrap();
    file.release(slot.active_extent).unwrap();
    file.checkpoint().unwrap();
    drop(file);

    // The reopened file is a reader's. It takes the extent back, in memory,
    // the first time it looks the segment up; a writer's open takes it
    // back for good.
    let reopened = fixture.container();
    assert!(!reopened.shared.file().is_writable());
    assert_eq!(reopened.holds(slot.active_extent), None);
    assert_eq!(
        reopened.segment_numbers().unwrap(),
        vec![slot.active_sequence]
    );
    assert_eq!(
        reopened.holds(slot.active_extent),
        Some((SEGMENT, reopened.shared.collection, slot.active_sequence))
    );
    assert_eq!(fixture.bodies(), before);
    put(&writer, 20);
    assert!(reopened.shared.file().is_writable());
    assert_eq!(
        reopened.holds(slot.active_extent),
        Some((SEGMENT, reopened.shared.collection, slot.active_sequence))
    );
    for number in 21..300 {
        put(&writer, number);
    }
    let bodies = fixture.bodies();
    assert_eq!(bodies.len(), 300);
    assert_eq!(bodies[&identifier(3)], before[&identifier(3)]);
    assert!(reopened.segment_numbers().unwrap().len() >= 2);
    assert!(reopened.is_mapped(slot.active_extent));
}

#[test]
fn a_compacted_extent_is_reused_after_a_checkpoint_and_old_readers_retry() {
    let fixture = Fixture::create("fxdb-medium-compact");
    let writer = fixture.writer();
    for number in 0..250 {
        put(&writer, number);
    }
    for number in 0..120 {
        patch(&writer, number, 1);
    }
    let container = fixture.container();
    let reader = SegmentStore::open(fixture.medium()).unwrap();
    let old = reader
        .location(&identifier(200), LocationState::Live)
        .unwrap()
        .clone();
    let old_extent = container.extent_of(old.segment);
    assert_ne!(old.segment, container.generation().active_sequence);
    reader
        .document(&identifier(200), LocationState::Live)
        .unwrap();
    let before = fixture.bodies();

    let report = writer.compact_collection("items").unwrap();
    assert!(report.segments_compacted >= 1, "{report:?}");
    assert!(!container.segment_numbers().unwrap().contains(&old.segment));
    // The release is checkpointed at once, so the extent may be reused by
    // anything, even the map checkpoint that freed it.
    let reused = |holds: Option<(ContainerExtentKind, u32, u64)>| {
        holds.is_some_and(|(_, _, sequence)| sequence > old.segment)
    };
    assert_ne!(
        container.holds(old_extent),
        Some((SEGMENT, container.shared.collection, old.segment)),
        "released"
    );
    assert_eq!(fixture.bodies(), before);

    let mut number = 1_000;
    while !reused(container.holds(old_extent)) {
        assert!(number < 2_000, "the released extent was never reused");
        put(&writer, number);
        number += 1;
    }
    let error = reader
        .document(&identifier(200), LocationState::Live)
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::ConcurrentWrite);
    let fresh = SegmentStore::open(fixture.medium()).unwrap();
    let document = fresh
        .document(&identifier(200), LocationState::Live)
        .unwrap();
    assert_ne!(document.location.segment, old.segment);
    let bodies = fixture.bodies();
    assert_eq!(bodies[&identifier(200)], before[&identifier(200)]);
    assert_eq!(bodies[&identifier(5)], before[&identifier(5)]);
}

#[test]
fn a_replaced_snapshot_and_the_replayed_records_give_the_table() {
    let fixture = Fixture::create("fxdb-medium-snapshot");
    let writer = fixture.writer();
    for number in 0..200 {
        put(&writer, number);
    }
    let container = fixture.container();
    let first = container
        .shared
        .file()
        .read_checkpoint(container.shared.block)
        .unwrap()
        .expect("a rotation wrote a snapshot");
    let first_run = first.locations.unwrap();
    let first_holder = container.holds(first_run.first_extent);
    assert_eq!(first_holder.map(|(kind, ..)| kind), Some(LOCATIONS));
    for number in 200..230 {
        put(&writer, number);
    }
    for number in (0..230).step_by(7) {
        patch(&writer, number, 2);
    }
    for number in (0..230).step_by(11) {
        writer
            .delete_document("items", &identifier(number), None)
            .unwrap();
    }

    let medium = fixture.medium();
    let slot = medium.require_slot().unwrap();
    let end = RecordPosition::published(&slot);
    TableCache::shared().forget(&*medium);
    let loaded = LocationTable::load(&*medium, TableReach::published(&*medium, &slot)).unwrap();
    assert_eq!(loaded.reach(), TableReach::Records(end));
    let mut replayed = LocationTable::default();
    replayed
        .replay(&*medium, (RecordPosition::START, end))
        .unwrap();
    assert_eq!(
        loaded.encode_snapshot().unwrap(),
        replayed.encode_snapshot().unwrap()
    );
    assert_eq!(
        loaded.get(&identifier(11)).unwrap().state,
        LocationState::Deleted
    );

    writer.rebuild_collection("items").unwrap();
    let second = container
        .shared
        .file()
        .read_checkpoint(container.shared.block)
        .unwrap()
        .unwrap();
    assert!(second.checkpoint > first.checkpoint);
    assert_ne!(second.locations, Some(first_run));
    assert_ne!(
        container.holds(first_run.first_extent),
        first_holder,
        "the replaced run was released"
    );
    let (snapshot, position) = medium.read_location_checkpoint().unwrap();
    let slot = medium.require_slot().unwrap();
    assert!(position <= RecordPosition::published(&slot));
    let table = SegmentStore::open(Arc::clone(&medium)).unwrap();
    let mut rebuilt = LocationTable::default();
    rebuilt
        .replay(
            &*medium,
            (RecordPosition::START, RecordPosition::published(&slot)),
        )
        .unwrap();
    assert_eq!(
        table.table().encode_snapshot().unwrap(),
        rebuilt.encode_snapshot().unwrap()
    );
    assert!(!snapshot.is_empty());
    let ids = table.ids(LocationState::Live);
    assert_eq!(ids.len(), 230 - (0..230).step_by(11).count());
}

#[test]
fn finished_segments_are_sealed_in_the_durable_map() {
    let fixture = Fixture::create("fxdb-medium-seal");
    let writer = fixture.writer();
    for number in 0..300 {
        put(&writer, number);
    }
    let container = fixture.container();
    let numbers = container.segment_numbers().unwrap();
    assert!(numbers.len() >= 2, "{numbers:?}");
    let (finished, active) = numbers.split_at(numbers.len() - 1);
    for number in finished {
        let extent = container.extent_of(*number);
        let live = container
            .sealed(extent)
            .expect("a finished segment is sealed");
        assert!(
            live > 0 && live <= container.segment_length(*number).unwrap(),
            "{number}: {live}"
        );
    }
    assert_eq!(container.sealed(container.extent_of(active[0])), None);
    let census = container.shared.file().allocator().census();
    assert_eq!(census.by_kind["segment"].sealed, finished.len() as u64);
    drop(container);
    fixture.crash();

    // The seals are in the map on disk, so a fresh open sees them, and a
    // shrink may move the sealed extents but never the active one.
    let reopened = fixture.container();
    for number in finished {
        assert!(reopened.sealed(reopened.extent_of(*number)).is_some());
    }
    let active_extent = reopened.extent_of(active[0]);
    let plan = reopened.shared.file().allocator().shrink_plan();
    assert!(plan.moves.iter().all(|(from, _)| *from != active_extent));

    // A compaction's copy is finished once it is adopted.
    for number in 0..120 {
        patch(&writer, number, 1);
    }
    let report = writer.compact_collection("items").unwrap();
    assert!(report.segments_compacted >= 1, "{report:?}");
    let active = reopened.generation().active_sequence;
    let copies: Vec<u64> = reopened
        .segment_numbers()
        .unwrap()
        .into_iter()
        .filter(|number| *number != active && !finished.contains(number))
        .collect();
    assert!(!copies.is_empty());
    for number in copies {
        let live = reopened.sealed(reopened.extent_of(number));
        assert!(live.is_some_and(|live| live > 0), "{number}: {live:?}");
    }
}

#[test]
fn the_segment_table_follows_takes_releases_and_reopens() {
    let fixture = Fixture::create("fxdb-medium-table");
    let writer = fixture.writer();
    let container = fixture.container();
    for number in 0..250 {
        put(&writer, number);
    }
    let (table, mapped) = container.tables();
    assert!(table.len() >= 2, "{table:?}");
    assert_eq!(table, mapped);
    assert_eq!(
        container.segment_numbers().unwrap(),
        table.keys().copied().collect::<Vec<_>>()
    );
    for number in 0..120 {
        patch(&writer, number, 1);
    }
    writer.compact_collection("items").unwrap();
    let (table, mapped) = container.tables();
    assert_eq!(
        table, mapped,
        "compaction took a copy and released segments"
    );
    drop(container);
    fixture.crash();

    // A fresh open builds the table from the map and the active extent its
    // slot names. Each release was checkpointed, so it outlived the open.
    let reopened = fixture.container();
    let (rebuilt, mapped) = reopened.tables();
    assert_eq!(rebuilt, mapped);
    assert_eq!(rebuilt, table);
    assert_eq!(fixture.bodies().len(), 250);
}

#[test]
fn a_dropped_collection_recreated_in_one_process_gets_a_fresh_file() {
    let fixture = Fixture::create("fxdb-medium-recreate");
    let writer = fixture.writer();
    for number in 0..20 {
        put(&writer, number);
    }
    let container = fixture.container();
    assert!(
        Arc::ptr_eq(&container.shared, &fixture.container().shared),
        "media open at once share one open"
    );
    let old = Arc::downgrade(&container.shared);
    let old_identity = container.identity();
    drop(container);
    assert!(old.upgrade().is_none(), "the registry keeps no medium open");

    writer.drop_collection("items").unwrap();
    writer
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    let fresh = fixture.container();
    assert_ne!(fresh.identity(), old_identity);
    assert!(fixture.bodies().is_empty());
    put(&writer, 7);
    let bodies = fixture.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[&identifier(7)]["name"], "user7");
}

/// Where [`damage`] flips a bit of a record's frame: its checksum, which
/// leaves the stored body readable, or its codec header byte, which leaves
/// nothing readable.
const CHECKSUM: usize = LogSegmentKind::MARKER_BYTES + 4;
const CODEC_HEADER: usize = LogSegmentKind::MARKER_BYTES + LogRecord::FRAME_BYTES;

/// Flip one bit `at` bytes into the frame of the active segment's first
/// record that holds `needle`, as a bad sector would. Records are
/// compressed, so the record is found by decoding them.
fn damage(fixture: &Fixture, needle: &[u8], at: usize) {
    let file = ContainerFile::open_unchecked(&fixture.file()).unwrap();
    let block = file.collections()[0].block;
    let slot = file.read_generation(block).unwrap().unwrap();
    let mut bytes = vec![0; usize::try_from(slot.committed_length).unwrap()];
    file.read_payload(slot.active_extent, 0, &mut bytes)
        .unwrap();
    let scan = LogScan::segment_with(&bytes, &*fixture.medium().frame_bodies()).unwrap();
    let holds = |part: &[u8]| part.windows(needle.len()).any(|window| window == needle);
    let (offset, _) = scan
        .records
        .iter()
        .find(|(_, record)| holds(&record.manifest) || holds(&record.blobs))
        .unwrap();
    let at = usize::try_from(*offset).unwrap() + at;
    file.write_payload(slot.active_extent, at as u64, &[bytes[at] ^ 0x20])
        .unwrap();
    file.sync().unwrap();
}

/// Salvage the collection in a process that has no table yet, as `fylo
/// salvage` run after a stop does.
fn fresh_salvage(fixture: &Fixture) -> crate::SalvageReport {
    fixture.crash();
    let out = std::env::temp_dir().join(crate::write::unique_name("fxdb-salvaged"));
    let report = fixture.writer().salvage_collection("items", &out).unwrap();
    let _ = std::fs::remove_dir_all(out);
    report
}

#[test]
fn a_fresh_salvage_names_the_documents_of_a_damaged_record_after_the_snapshot() {
    let fixture = Fixture::create("fxdb-medium-salvage");
    let writer = fixture.writer();
    for number in 0..10 {
        put(&writer, number);
    }
    damage(&fixture, br#""user4""#, CHECKSUM);
    fixture.crash();
    let error = SegmentStore::open(fixture.medium()).unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::CorruptDocument);

    // As a directory collection's location log would, the damaged record's
    // manifest names the document whose only version it held.
    let report = fresh_salvage(&fixture);
    assert_eq!(report.recovered, 9, "{report:?}");
    assert_eq!(report.lost, vec![identifier(4)]);
    assert_eq!(report.damaged.len(), 1, "{report:?}");

    fixture.crash();
    let verification = writer.verify_segments("items").unwrap();
    assert_eq!(verification.documents, 4, "the table stops at the damage");
    let defects = verification.defects.join("\n");
    assert!(defects.contains("locations stop at segment"), "{defects}");
    assert!(
        defects.contains(&format!("the newest version of {}", identifier(4))),
        "{defects}"
    );
    assert!(!defects.contains(&identifier(5)), "{defects}");
}

#[test]
fn a_fresh_salvage_names_nothing_written_again_or_unreadable() {
    // A document written again after the damaged record is recovered from
    // its later version and is not lost.
    let fixture = Fixture::create("fxdb-medium-salvage-again");
    let writer = fixture.writer();
    for number in 0..10 {
        put(&writer, number);
    }
    patch(&writer, 4, 1);
    damage(&fixture, br#""user4""#, CHECKSUM);
    let report = fresh_salvage(&fixture);
    assert_eq!(report.recovered, 10, "{report:?}");
    assert!(report.lost.is_empty(), "{report:?}");

    // A damaged manifest names nothing: only the damaged range is
    // reported, where a location log would still name the document.
    let fixture = Fixture::create("fxdb-medium-salvage-manifest");
    let writer = fixture.writer();
    for number in 0..10 {
        put(&writer, number);
    }
    damage(&fixture, identifier(4).as_bytes(), CODEC_HEADER);
    let report = fresh_salvage(&fixture);
    assert_eq!(report.recovered, 9, "{report:?}");
    assert!(report.lost.is_empty(), "{report:?}");
    assert_eq!(report.damaged.len(), 1, "{report:?}");
}
