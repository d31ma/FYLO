use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use fylo_storage_native::{
    CollectionKind, CollectionLayout, NativeRoot, NativeWriteRoot, PutDocumentOptions, RootConfig,
};
use serde_json::{Map, Value, json};

struct Fixture(PathBuf);

impl Fixture {
    fn create(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-seg-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        let fixture = Self(fs::canonicalize(path).unwrap());
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

    fn collection(&self) -> fylo_storage_native::NativeCollection {
        NativeRoot::open(&self.0)
            .unwrap()
            .collection("items")
            .unwrap()
    }

    fn segments(&self) -> PathBuf {
        self.0.join(".collections/items/segments")
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

    fn mark_writing(&self) {
        let slots = self.segments().join("state.slots");
        let mut bytes = fs::read(&slots).unwrap();
        for slot in bytes.chunks_mut(64) {
            slot.fill(0);
        }
        fs::write(&slots, bytes).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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

#[test]
fn a_new_collection_uses_segments_and_no_document_files() {
    let fixture = Fixture::create("layout");
    let writer = fixture.writer();
    for number in 0..20 {
        put(&writer, number);
    }
    assert_eq!(fixture.collection().layout(), CollectionLayout::Segments);
    assert!(fixture.0.join(".collections/items/docs").is_file());
    assert!(fixture.segments().join("0000000001.seg").is_file());
    let bodies = fixture.bodies();
    assert_eq!(bodies.len(), 20);
    assert_eq!(bodies[&identifier(3)]["name"], "user3");
    let marker: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .0
                .join(".fylo-transactions/.collections/items/state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(marker["format"], "fylo.collection-segments.v1");
    fixture.index_matches_rebuild();
}

#[test]
fn patch_delete_and_restore_follow_the_file_layout_contract() {
    let fixture = Fixture::create("cycle");
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

#[test]
fn batch_reads_return_documents_in_the_order_asked() {
    let fixture = Fixture::create("batch");
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

#[test]
fn recovery_replays_commits_after_the_slots_are_lost() {
    let fixture = Fixture::create("recover");
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

#[test]
fn recovery_rebuilds_lost_location_files_from_segments() {
    let fixture = Fixture::create("rebuild");
    let writer = fixture.writer();
    for number in 0..10 {
        put(&writer, number);
    }
    writer
        .delete_document("items", &identifier(4), None)
        .unwrap();
    let before = fixture.bodies();
    fs::write(fixture.segments().join("locations.wal"), b"").unwrap();
    fs::write(
        fixture.segments().join("locations.snapshot"),
        b"garbage-garbage",
    )
    .unwrap();
    fixture.mark_writing();
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fixture.bodies(), before);
    assert_eq!(
        fixture.collection().deleted_document_ids().unwrap(),
        vec![identifier(4)]
    );
}

#[test]
fn a_torn_append_is_cut_off_by_recovery() {
    let fixture = Fixture::create("torn");
    let writer = fixture.writer();
    for number in 0..5 {
        put(&writer, number);
    }
    let before = fixture.bodies();
    let segment = fixture.segments().join("0000000001.seg");
    let mut bytes = fs::read(&segment).unwrap();
    bytes.extend_from_slice(&[0xF5, 0x59, 0x52, 0x01, 200, 0, 0, 0, 1, 2]);
    fs::write(&segment, bytes).unwrap();
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fixture.bodies(), before);
    put(&writer, 9);
    assert_eq!(fixture.bodies().len(), 6);
}

#[test]
fn concurrent_writers_share_the_segment() {
    let fixture = Fixture::create("concurrent");
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

#[test]
fn checkpoints_fold_locations_and_rotate_nothing_small() {
    let fixture = Fixture::create("checkpoint");
    let writer = fixture.writer();
    for number in 0..1_200 {
        put(&writer, number);
    }
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
    assert_eq!(fixture.bodies().len(), 1_200);
    fixture.index_matches_rebuild();
}

fn segment_files(fixture: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(fixture.segments())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| {
            std::path::Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("seg"))
        })
        .collect();
    names.sort();
    names
}

#[test]
fn compaction_keeps_every_document_and_reclaims_dead_versions() {
    let fixture = Fixture::create("compact");
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
    assert_eq!(
        segment_files(&fixture).len(),
        2,
        "{:?}",
        segment_files(&fixture)
    );
    fixture.index_matches_rebuild();
    put(&writer, 99);
    assert_eq!(fixture.bodies().len(), before.len() + 1);
    let again = writer.compact_collection("items").unwrap();
    assert!(again.documents_copied >= 40);
    assert_eq!(
        segment_files(&fixture).len(),
        2,
        "{:?}",
        segment_files(&fixture)
    );
}

#[test]
fn a_rebuild_after_compaction_matches_the_location_table() {
    let fixture = Fixture::create("compact-rebuild");
    let writer = fixture.writer();
    for number in 0..30 {
        put(&writer, number);
    }
    writer
        .patch_document("items", &identifier(2), br#"{"name":"patched"}"#, None)
        .unwrap();
    writer.compact_collection("items").unwrap();
    let before = fixture.bodies();
    fs::write(fixture.segments().join("locations.snapshot"), b"").unwrap();
    fs::write(fixture.segments().join("locations.wal"), b"").unwrap();
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
    let fixture = Fixture(fs::canonicalize(path).unwrap());
    let files = NativeWriteRoot::open(&fixture.0).unwrap();
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
    let original = file_tree(&fixture.0);
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
    assert!(fixture.0.join(".collections/items/docs").is_file());
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
    let mut round_trip = file_tree(&fixture.0);
    round_trip.retain(|path, _| !path.contains(&identifier(50)));
    assert_eq!(round_trip, original);
    assert_eq!(fixture.bodies(), bodies);
    let state: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .0
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

#[test]
fn export_writes_one_file_per_document() {
    let fixture = Fixture::create("export");
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

#[test]
fn verify_finds_damage_and_salvage_keeps_the_rest() {
    let fixture = Fixture::create("salvage");
    let writer = fixture.writer();
    for number in 0..10 {
        put(&writer, number);
    }
    assert!(writer.verify_segments("items").unwrap().defects.is_empty());
    let segment = fixture.segments().join("0000000001.seg");
    let mut bytes = fs::read(&segment).unwrap();
    let needle = br#""user4""#;
    let at = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    bytes[at + 2] ^= 0x20;
    fs::write(&segment, &bytes).unwrap();
    let verification = writer.verify_segments("items").unwrap();
    assert!(!verification.defects.is_empty(), "{verification:?}");
    let out = scratch("salvage");
    let report = writer.salvage_collection("items", &out).unwrap();
    assert_eq!(report.recovered, 9, "{report:?}");
    assert_eq!(report.lost, vec![identifier(4)]);
    let _ = fs::remove_dir_all(out);
}

#[test]
fn recovery_refuses_to_cut_committed_records_after_damage() {
    let fixture = Fixture::create("midcut");
    let writer = fixture.writer();
    for number in 0..6 {
        put(&writer, number);
    }
    let segment = fixture.segments().join("0000000001.seg");
    let mut bytes = fs::read(&segment).unwrap();
    let needle = br#""user2""#;
    let at = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    bytes[at + 2] ^= 0x20;
    fs::write(&segment, &bytes).unwrap();
    fixture.mark_writing();
    let error = writer.recover_collection("items").unwrap_err();
    assert!(error.to_string().contains("salvage"), "{error}");
    assert_eq!(fs::read(&segment).unwrap(), bytes);
}
