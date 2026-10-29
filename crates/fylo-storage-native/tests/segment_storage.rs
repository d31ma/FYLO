//! The segment collection suite, run over both media: a collection
//! directory's `segments/` and a `.fxdb` container in the collection
//! directory (`docs/fxdb-segment-backend.md`).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use fylo_format::{
    ContainerCheckpointSlot, ContainerCollectionHeader, ContainerExtentKind,
    ContainerGenerationSlot,
};
use fylo_storage_native::{
    CollectionKind, CollectionLayout, ContainerFile, NativeRoot, NativeWriteRoot,
    PutDocumentOptions, RootConfig,
};
use serde_json::{Map, Value, json};

/// Where a segment collection keeps its segments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Medium {
    Directory,
    Container,
}

/// Runs each test once per medium, as `directory::name` and
/// `container::name`.
macro_rules! over_both_media {
    ($($name:ident),* $(,)?) => {
        mod directory {
            $(
                #[test]
                fn $name() {
                    super::$name(super::Medium::Directory);
                }
            )*
        }

        mod container {
            $(
                #[test]
                fn $name() {
                    super::$name(super::Medium::Container);
                }
            )*
        }
    };
}

over_both_media!(
    a_new_collection_uses_segments_and_no_document_files,
    patch_delete_and_restore_follow_the_file_layout_contract,
    batch_reads_return_documents_in_the_order_asked,
    recovery_replays_commits_after_the_slots_are_lost,
    recovery_rebuilds_lost_location_files_from_segments,
    a_torn_append_is_cut_off_by_recovery,
    concurrent_writers_share_the_segment,
    checkpoints_fold_locations_and_rotate_nothing_small,
    compaction_keeps_every_document_and_reclaims_dead_versions,
    a_rebuild_after_compaction_matches_the_location_table,
    export_writes_one_file_per_document,
    verify_finds_damage_and_salvage_keeps_the_rest,
    recovery_refuses_to_cut_committed_records_after_damage,
);

struct Fixture {
    root: PathBuf,
    medium: Medium,
}

impl Fixture {
    fn create(label: &str, medium: Medium) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-seg-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        let fixture = Self {
            root: fs::canonicalize(path).unwrap(),
            medium,
        };
        fixture
            .writer()
            .create_collection("items", CollectionKind::Document, None)
            .unwrap();
        fixture
    }

    fn writer(&self) -> NativeWriteRoot {
        let writer = NativeWriteRoot::open(&self.root)
            .unwrap()
            .with_config(RootConfig {
                layout: CollectionLayout::Segments,
                ..RootConfig::default()
            });
        match self.medium {
            Medium::Directory => writer,
            Medium::Container => writer.with_segment_container(),
        }
    }

    fn collection(&self) -> fylo_storage_native::NativeCollection {
        NativeRoot::open(&self.root)
            .unwrap()
            .collection("items")
            .unwrap()
    }

    fn segments(&self) -> PathBuf {
        self.root.join(".collections/items/segments")
    }

    fn container(&self) -> ContainerFile {
        ContainerFile::open_unchecked(&self.root.join(".collections/items/segments.fxdb")).unwrap()
    }

    fn bodies(&self) -> BTreeMap<String, Value> {
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

    fn index_matches_rebuild(&self) {
        let before = self
            .collection()
            .index_snapshot()
            .unwrap()
            .as_bytes()
            .to_vec();
        self.writer().rebuild_collection("items").unwrap();
        let rebuilt = self
            .collection()
            .index_snapshot()
            .unwrap()
            .as_bytes()
            .to_vec();
        assert_eq!(sorted_lines(&before), sorted_lines(&rebuilt));
    }

    /// Lose both generation slots, which leaves the collection writing.
    fn mark_writing(&self) {
        match self.medium {
            Medium::Directory => {
                let slots = self.segments().join("state.slots");
                let mut bytes = fs::read(&slots).unwrap();
                for slot in bytes.chunks_mut(64) {
                    slot.fill(0);
                }
                fs::write(&slots, bytes).unwrap();
            }
            Medium::Container => self.zero_slot_pair(
                ContainerCollectionHeader::GENERATION_SLOTS_OFFSET,
                ContainerGenerationSlot::ENCODED_BYTES,
            ),
        }
    }

    /// Lose the location snapshot, and the location log where there is one.
    fn lose_locations(&self, snapshot: &[u8]) {
        match self.medium {
            Medium::Directory => {
                fs::write(self.segments().join("locations.wal"), b"").unwrap();
                fs::write(self.segments().join("locations.snapshot"), snapshot).unwrap();
            }
            Medium::Container => self.zero_slot_pair(
                ContainerCollectionHeader::CHECKPOINT_SLOTS_OFFSET,
                ContainerCheckpointSlot::ENCODED_BYTES,
            ),
        }
    }

    fn zero_slot_pair(&self, within: usize, slot_bytes: usize) {
        let container = self.container();
        let block = container.collections()[0].block;
        let at = u64::from(block) * ContainerCollectionHeader::BLOCK_BYTES as u64 + within as u64;
        container
            .write_payload(
                container.superblock().directory_extent,
                at,
                &vec![0; 2 * slot_bytes],
            )
            .unwrap();
        container.sync().unwrap();
    }

    /// The active segment, while it is the only one.
    fn active_segment(&self) -> Segment {
        match self.medium {
            Medium::Directory => Segment::File(self.segments().join("0000000001.seg")),
            Medium::Container => {
                let container = self.container();
                let slot = container
                    .read_generation(container.collections()[0].block)
                    .unwrap()
                    .unwrap();
                Segment::Extent {
                    container: self.root.join(".collections/items/segments.fxdb"),
                    extent: slot.active_extent,
                    sequence: slot.active_sequence,
                    length: slot.committed_length,
                }
            }
        }
    }

    /// Segments the collection holds.
    fn segment_count(&self) -> usize {
        self.writer().verify_segments("items").unwrap().segments
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The bytes of one segment, as a file or as an extent's payload up to the
/// committed length its slot published.
enum Segment {
    File(PathBuf),
    Extent {
        container: PathBuf,
        extent: u64,
        sequence: u64,
        length: u64,
    },
}

impl Segment {
    fn read(&self) -> Vec<u8> {
        match self {
            Self::File(path) => fs::read(path).unwrap(),
            Self::Extent { length, .. } => {
                let mut bytes = vec![0; usize::try_from(*length).unwrap()];
                self.open()
                    .read_payload(self.extent(), 0, &mut bytes)
                    .unwrap();
                bytes
            }
        }
    }

    /// Write `bytes` from the segment's start; past its end they are a
    /// tail no commit wrote.
    fn write(&self, bytes: &[u8]) {
        match self {
            Self::File(path) => fs::write(path, bytes).unwrap(),
            Self::Extent { .. } => {
                let container = self.open();
                container.write_payload(self.extent(), 0, bytes).unwrap();
                container.sync().unwrap();
            }
        }
    }

    fn extent(&self) -> u64 {
        match self {
            Self::File(_) => unreachable!("a segment file has no extent"),
            Self::Extent { extent, .. } => *extent,
        }
    }

    /// The container, with the extent in its map: a new segment's extent
    /// is only in the map the next map checkpoint writes, and a handle that
    /// opens before then takes it back from the slot that names it.
    fn open(&self) -> ContainerFile {
        let Self::Extent {
            container,
            extent,
            sequence,
            ..
        } = self
        else {
            unreachable!("a segment file is no container");
        };
        let mut container = ContainerFile::open_unchecked(container).unwrap();
        if container.allocator().find_sequence(*sequence).is_none() {
            let owner = container.collections()[0].header.id;
            container
                .reclaim(*extent, ContainerExtentKind::Segment, owner, *sequence)
                .unwrap();
        }
        container
    }
}

fn sorted_lines(bytes: &[u8]) -> Vec<String> {
    let mut lines: Vec<String> = String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 20_000 + number)
}

fn put(writer: &NativeWriteRoot, number: usize) {
    let body = json!({ "group": number % 7, "name": format!("user{number}") }).to_string();
    writer
        .put_document(
            "items",
            &identifier(number),
            body.as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn a_new_collection_uses_segments_and_no_document_files(medium: Medium) {
    let fixture = Fixture::create("layout", medium);
    let writer = fixture.writer();
    for number in 0..20 {
        put(&writer, number);
    }
    assert_eq!(fixture.collection().layout(), CollectionLayout::Segments);
    assert!(fixture.root.join(".collections/items/docs").is_file());
    match medium {
        Medium::Directory => assert!(fixture.segments().join("0000000001.seg").is_file()),
        Medium::Container => {
            assert!(
                fixture
                    .root
                    .join(".collections/items/segments.fxdb")
                    .is_file()
            );
            assert!(!fixture.segments().exists());
        }
    }
    let bodies = fixture.bodies();
    assert_eq!(bodies.len(), 20);
    assert_eq!(bodies[&identifier(3)]["name"], "user3");
    let marker: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .root
                .join(".fylo-transactions/.collections/items/state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(marker["format"], "fylo.collection-segments.v1");
    fixture.index_matches_rebuild();
}

fn patch_delete_and_restore_follow_the_file_layout_contract(medium: Medium) {
    let fixture = Fixture::create("cycle", medium);
    let writer = fixture.writer();
    put(&writer, 1);
    put(&writer, 2);
    writer
        .patch_document("items", &identifier(1), br#"{"name":"renamed"}"#, None)
        .unwrap();
    writer
        .delete_document("items", &identifier(2), None)
        .unwrap();
    let collection = fixture.collection();
    assert_eq!(collection.document_ids().unwrap(), vec![identifier(1)]);
    assert_eq!(
        collection.deleted_document_ids().unwrap(),
        vec![identifier(2)]
    );
    assert_eq!(fixture.bodies()[&identifier(1)]["name"], "renamed");
    assert!(collection.read_document(&identifier(2)).is_err());
    writer
        .restore_document("items", &identifier(2), None)
        .unwrap();
    assert_eq!(fixture.bodies().len(), 2);
    let duplicate = writer.put_document(
        "items",
        &identifier(1),
        br#"{"name":"again"}"#,
        PutDocumentOptions::default(),
    );
    assert!(duplicate.is_err());
    fixture.index_matches_rebuild();
}

fn batch_reads_return_documents_in_the_order_asked(medium: Medium) {
    let fixture = Fixture::create("batch", medium);
    let writer = fixture.writer();
    for number in 0..50 {
        put(&writer, number);
    }
    let wanted: Vec<String> = [40, 3, 17, 3, 0].iter().map(|n| identifier(*n)).collect();
    let documents = fixture.collection().read_documents(&wanted).unwrap();
    let names: Vec<Value> = documents
        .iter()
        .map(|stored| serde_json::from_slice::<Value>(&stored.bytes).unwrap()["name"].clone())
        .collect();
    assert_eq!(names, vec!["user40", "user3", "user17", "user3", "user0"]);
}

fn recovery_replays_commits_after_the_slots_are_lost(medium: Medium) {
    let fixture = Fixture::create("recover", medium);
    let writer = fixture.writer();
    for number in 0..10 {
        put(&writer, number);
    }
    let before = fixture.bodies();
    fixture.mark_writing();
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fixture.bodies(), before);
    assert!(!writer.recover_collection("items").unwrap());
    fixture.index_matches_rebuild();
}

fn recovery_rebuilds_lost_location_files_from_segments(medium: Medium) {
    let fixture = Fixture::create("rebuild", medium);
    let writer = fixture.writer();
    for number in 0..10 {
        put(&writer, number);
    }
    writer
        .delete_document("items", &identifier(4), None)
        .unwrap();
    let before = fixture.bodies();
    fixture.lose_locations(b"garbage-garbage");
    fixture.mark_writing();
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fixture.bodies(), before);
    assert_eq!(
        fixture.collection().deleted_document_ids().unwrap(),
        vec![identifier(4)]
    );
}

fn a_torn_append_is_cut_off_by_recovery(medium: Medium) {
    let fixture = Fixture::create("torn", medium);
    let writer = fixture.writer();
    for number in 0..5 {
        put(&writer, number);
    }
    let before = fixture.bodies();
    let segment = fixture.active_segment();
    let mut bytes = segment.read();
    bytes.extend_from_slice(&[0xF5, 0x59, 0x52, 0x01, 200, 0, 0, 0, 1, 2]);
    segment.write(&bytes);
    if medium == Medium::Container {
        // A container flushes an append together with the `writing` slot
        // published before it, so a torn tail always comes with one; a
        // segment file's longer length is what shows it otherwise.
        fixture.mark_writing();
    }
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fixture.bodies(), before);
    put(&writer, 9);
    assert_eq!(fixture.bodies().len(), 6);
}

fn concurrent_writers_share_the_segment(medium: Medium) {
    let fixture = Fixture::create("concurrent", medium);
    let writer = Arc::new(fixture.writer());
    let threads: Vec<_> = (0..4)
        .map(|thread_number| {
            let writer = Arc::clone(&writer);
            thread::spawn(move || {
                for number in 0..25 {
                    put(&writer, thread_number * 100 + number);
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(fixture.bodies().len(), 100);
    fixture.index_matches_rebuild();
}

fn checkpoints_fold_locations_and_rotate_nothing_small(medium: Medium) {
    let fixture = Fixture::create("checkpoint", medium);
    let writer = fixture.writer();
    for number in 0..1_200 {
        put(&writer, number);
    }
    if medium == Medium::Directory {
        let wal = fs::metadata(fixture.segments().join("locations.wal"))
            .unwrap()
            .len();
        assert!(wal <= 64 * 1024 + 72, "location log was not folded: {wal}");
        assert!(
            fs::metadata(fixture.segments().join("locations.snapshot"))
                .unwrap()
                .len()
                > 0
        );
    }
    assert_eq!(fixture.bodies().len(), 1_200);
    fixture.index_matches_rebuild();
    if medium == Medium::Container {
        // No location log: every checkpoint writes the snapshot, and the
        // rebuild's closing one names it.
        let container = fixture.container();
        let checkpoint = container
            .read_checkpoint(container.collections()[0].block)
            .unwrap()
            .unwrap();
        assert!(checkpoint.locations.is_some_and(|run| run.length > 0));
    }
}

fn compaction_keeps_every_document_and_reclaims_dead_versions(medium: Medium) {
    let fixture = Fixture::create("compact", medium);
    let writer = fixture.writer();
    for number in 0..40 {
        put(&writer, number);
    }
    for round in 0..5 {
        for number in 0..40 {
            let patch = json!({ "round": round }).to_string();
            writer
                .patch_document("items", &identifier(number), patch.as_bytes(), None)
                .unwrap();
        }
    }
    writer
        .delete_document("items", &identifier(7), None)
        .unwrap();
    let before = fixture.bodies();
    let deleted = fixture.collection().deleted_document_ids().unwrap();
    let report = writer.compact_collection("items").unwrap();
    assert!(report.documents_copied >= 40, "{report:?}");
    assert!(report.bytes_after < report.bytes_before, "{report:?}");
    assert_eq!(fixture.bodies(), before);
    assert_eq!(
        fixture.collection().deleted_document_ids().unwrap(),
        deleted
    );
    assert_eq!(fixture.segment_count(), 2);
    fixture.index_matches_rebuild();
    put(&writer, 99);
    assert_eq!(fixture.bodies().len(), before.len() + 1);
    let again = writer.compact_collection("items").unwrap();
    assert!(again.documents_copied >= 40);
    assert_eq!(fixture.segment_count(), 2);
}

fn a_rebuild_after_compaction_matches_the_location_table(medium: Medium) {
    let fixture = Fixture::create("compact-rebuild", medium);
    let writer = fixture.writer();
    for number in 0..30 {
        put(&writer, number);
    }
    writer
        .patch_document("items", &identifier(2), br#"{"name":"patched"}"#, None)
        .unwrap();
    writer.compact_collection("items").unwrap();
    let before = fixture.bodies();
    fixture.lose_locations(b"");
    fixture.mark_writing();
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fixture.bodies(), before);
    assert_eq!(fixture.bodies()[&identifier(2)]["name"], "patched");
}

type FileTree = BTreeMap<String, (Vec<u8>, u128, Vec<(String, Vec<u8>)>)>;

fn file_tree(root: &std::path::Path) -> FileTree {
    let mut tree = BTreeMap::new();
    for namespace in ["docs", ".deleted"] {
        let base = root.join(".collections/items").join(namespace);
        for shard in fs::read_dir(&base).unwrap() {
            for entry in fs::read_dir(shard.unwrap().path()).unwrap() {
                let path = entry.unwrap().path();
                let metadata = fs::metadata(&path).unwrap();
                let modified = metadata
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis();
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                tree.insert(
                    relative,
                    (fs::read(&path).unwrap(), modified, attributes(&path)),
                );
            }
        }
    }
    tree
}

#[cfg(unix)]
fn attributes(path: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut values: Vec<(String, Vec<u8>)> = xattr::list(path)
        .unwrap()
        .filter_map(|name| {
            let name = name.to_string_lossy().into_owned();
            name.contains("fylo").then(|| {
                let value = xattr::get(path, &name).unwrap().unwrap_or_default();
                (name, value)
            })
        })
        .collect();
    values.sort();
    values
}

#[cfg(not(unix))]
fn attributes(_path: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    Vec::new()
}

/// A migration builds a segment directory, whatever medium new collections
/// get, so it runs once.
#[test]
fn migration_round_trips_a_file_collection_exactly() {
    let path = std::env::temp_dir().join(format!(
        "fylo-seg-migrate-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&path).unwrap();
    let fixture = Fixture {
        root: fs::canonicalize(path).unwrap(),
        medium: Medium::Directory,
    };
    let files = NativeWriteRoot::open(&fixture.root).unwrap();
    files
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    for number in 0..30 {
        put(&files, number);
    }
    let mut metadata = Map::new();
    metadata.insert("team".into(), json!("blue"));
    files
        .set_record_metadata("items", &identifier(5), &metadata, None)
        .unwrap();
    files
        .delete_document("items", &identifier(9), None)
        .unwrap();
    NativeWriteRoot::close_logs_under(files.path()).unwrap();
    let original = file_tree(&fixture.root);
    let bodies = fixture.bodies();

    let report = files
        .migrate_layout("items", CollectionLayout::Segments, false)
        .unwrap();
    assert_eq!((report.documents, report.deleted), (29, 1));
    assert_eq!(fixture.collection().layout(), CollectionLayout::Segments);
    assert_eq!(fixture.bodies(), bodies);
    assert_eq!(
        fixture
            .collection()
            .read_custom_metadata(&identifier(5))
            .unwrap()["team"],
        "blue"
    );
    assert!(fixture.root.join(".collections/items/docs").is_file());
    fixture.index_matches_rebuild();
    put(&files, 50);
    files
        .delete_document("items", &identifier(50), None)
        .unwrap();
    files
        .restore_document("items", &identifier(50), None)
        .unwrap();
    files
        .delete_document("items", &identifier(50), None)
        .unwrap();

    files
        .migrate_layout("items", CollectionLayout::Files, false)
        .unwrap();
    assert_eq!(fixture.collection().layout(), CollectionLayout::Files);
    let mut round_trip = file_tree(&fixture.root);
    round_trip.retain(|path, _| !path.contains(&identifier(50)));
    assert_eq!(round_trip, original);
    assert_eq!(fixture.bodies(), bodies);
    let state: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .root
                .join(".fylo-transactions/.collections/items/state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state["state"], "stable");
    fixture.index_matches_rebuild();
}

fn scratch(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fylo-seg-out-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn export_writes_one_file_per_document(medium: Medium) {
    let fixture = Fixture::create("export", medium);
    let writer = fixture.writer();
    for number in 0..12 {
        put(&writer, number);
    }
    writer
        .delete_document("items", &identifier(3), None)
        .unwrap();
    let out = scratch("export");
    let report = writer.export_collection("items", &out).unwrap();
    assert_eq!((report.documents, report.deleted), (11, 1));
    let mut files = 0;
    for namespace in ["docs", ".deleted"] {
        for shard in fs::read_dir(out.join(namespace)).unwrap() {
            files += fs::read_dir(shard.unwrap().path()).unwrap().count();
        }
    }
    assert_eq!(files, 12);
    assert!(writer.export_collection("items", &out).is_err());
    let _ = fs::remove_dir_all(out);
}

/// Flip one bit inside the record that holds `needle`.
fn damage(segment: &Segment, needle: &[u8]) -> Vec<u8> {
    let mut bytes = segment.read();
    let at = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    bytes[at + 2] ^= 0x20;
    segment.write(&bytes);
    bytes
}

fn verify_finds_damage_and_salvage_keeps_the_rest(medium: Medium) {
    let fixture = Fixture::create("salvage", medium);
    let writer = fixture.writer();
    for number in 0..10 {
        put(&writer, number);
    }
    assert!(writer.verify_segments("items").unwrap().defects.is_empty());
    damage(&fixture.active_segment(), br#""user4""#);
    let verification = writer.verify_segments("items").unwrap();
    assert!(!verification.defects.is_empty(), "{verification:?}");
    let out = scratch("salvage");
    let report = writer.salvage_collection("items", &out).unwrap();
    assert_eq!(report.recovered, 9, "{report:?}");
    // A container keeps no location log: the table this process built
    // before the damage names the lost document. A process that has to
    // replay the damaged record stops its table there and names it from
    // the damaged record's manifest (the container medium's tests).
    assert_eq!(report.lost, vec![identifier(4)]);
    let _ = fs::remove_dir_all(out);
}

fn recovery_refuses_to_cut_committed_records_after_damage(medium: Medium) {
    let fixture = Fixture::create("midcut", medium);
    let writer = fixture.writer();
    for number in 0..6 {
        put(&writer, number);
    }
    let segment = fixture.active_segment();
    let bytes = damage(&segment, br#""user2""#);
    fixture.mark_writing();
    let error = writer.recover_collection("items").unwrap_err();
    assert!(error.to_string().contains("salvage"), "{error}");
    assert_eq!(segment.read(), bytes);
}
