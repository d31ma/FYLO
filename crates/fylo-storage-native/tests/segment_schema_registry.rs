//! The schema registry of a segment collection (ADR 0017 section 3): each
//! version is a schema record (kind 5) in the collection's segments, and the
//! derived schemas snapshot lists them.

use std::fs;
use std::path::PathBuf;

use fylo_storage_native::{
    CollectionKind, CollectionLayout, GenerationState, NativeRoot, NativeStorageError,
    NativeStorageErrorCode, NativeWriteRoot, PutDocumentOptions, RootConfig,
};
use serde_json::{Value, json};

const MARKER: [u8; 4] = [0xF5, 0x59, 0x52, 0x01];

struct Fixture(PathBuf);

impl Fixture {
    fn create(label: &str, layout: CollectionLayout) -> Self {
        let path = scratch(label);
        fs::create_dir(&path).unwrap();
        let fixture = Self(fs::canonicalize(path).unwrap());
        fixture
            .writer_with(layout)
            .create_collection("items", CollectionKind::Document, None)
            .unwrap();
        fixture
    }

    fn segments(label: &str) -> Self {
        Self::create(label, CollectionLayout::Segments)
    }

    fn writer_with(&self, layout: CollectionLayout) -> NativeWriteRoot {
        NativeWriteRoot::open(&self.0)
            .unwrap()
            .with_config(RootConfig {
                layout,
                ..RootConfig::default()
            })
    }

    fn writer(&self) -> NativeWriteRoot {
        self.writer_with(CollectionLayout::Segments)
    }

    fn root(&self) -> NativeRoot {
        NativeRoot::open(&self.0).unwrap()
    }

    fn head(&self) -> Option<u32> {
        self.root().schema_registry_head("items").unwrap()
    }

    fn read(&self, ordinal: u32) -> Result<Vec<u8>, NativeStorageError> {
        self.root().read_schema_version("items", ordinal)
    }

    fn commit(&self, ordinal: u32) -> Result<(), NativeStorageError> {
        self.writer()
            .commit_schema_version("items", ordinal, version(ordinal).as_bytes(), |_| Ok(()))
    }

    fn segment_directory(&self) -> PathBuf {
        self.0.join(".collections/items/segments")
    }

    fn snapshot_path(&self) -> PathBuf {
        self.segment_directory().join("schemas.snapshot")
    }

    fn descriptor_path(&self) -> PathBuf {
        self.0.join(".fylo-catalog/collections/items.json")
    }

    fn descriptor(&self) -> Value {
        serde_json::from_slice(&fs::read(self.descriptor_path()).unwrap()).unwrap()
    }

    fn set_descriptor_head(&self, head: u32) {
        let mut descriptor = self.descriptor();
        descriptor["schemaHead"] = json!(head);
        fs::write(
            self.descriptor_path(),
            serde_json::to_vec(&descriptor).unwrap(),
        )
        .unwrap();
    }

    fn snapshot(&self) -> Value {
        serde_json::from_slice(&fs::read(self.snapshot_path()).unwrap()).unwrap()
    }

    fn segment_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(self.segment_directory())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "seg"))
            .collect();
        files.sort();
        files
    }

    fn active_segment(&self) -> PathBuf {
        self.segment_files().pop().unwrap()
    }

    /// Lose both generation slots, as a crash that tore them would.
    fn mark_writing(&self) {
        let slots = self.segment_directory().join("state.slots");
        let mut bytes = fs::read(&slots).unwrap();
        bytes.fill(0);
        fs::write(&slots, bytes).unwrap();
    }

    fn put(&self, number: usize, body: &Value) {
        self.writer()
            .put_document(
                "items",
                &identifier(number),
                body.to_string().as_bytes(),
                PutDocumentOptions::default(),
            )
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fylo-seg-schemas-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn version(ordinal: u32) -> String {
    format!("{{\n  \"ordinal\": {ordinal},\n  \"label\": \"v{ordinal}\"\n}}\n")
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 30_000 + number)
}

/// Offsets where each record frame of a segment starts.
fn frame_starts(bytes: &[u8]) -> Vec<usize> {
    bytes
        .windows(MARKER.len())
        .enumerate()
        .filter(|(_, window)| *window == MARKER)
        .map(|(offset, _)| offset)
        .collect()
}

#[test]
fn a_segment_collection_commits_versions_as_records() {
    let fixture = Fixture::segments("commit");
    assert_eq!(fixture.head(), None);
    fixture.commit(1).unwrap();
    fixture.commit(2).unwrap();
    assert_eq!(fixture.head(), Some(2));
    assert_eq!(fixture.read(1).unwrap(), version(1).as_bytes());
    assert_eq!(fixture.read(2).unwrap(), version(2).as_bytes());
    for ordinal in [0, 3] {
        assert_eq!(
            fixture.read(ordinal).unwrap_err().code(),
            NativeStorageErrorCode::NotFound
        );
    }
    // The records are the registry: no directory registry files, a
    // snapshot listing both, and the descriptor following the head.
    assert!(!fixture.0.join(".fylo-schemas/items").exists());
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot["format"], "fylo.schemas-snapshot.v1");
    assert_eq!(snapshot["versions"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["versions"][1]["version"], version(2));
    let descriptor = fixture.descriptor();
    assert_eq!(descriptor["schemas"], "registry", "{descriptor}");
    assert_eq!(descriptor["schemaHead"], 2, "{descriptor}");
    assert_eq!(descriptor["layout"], "segments", "{descriptor}");
    // Each version is a record of kind 5 in the active segment.
    let segment = fs::read(fixture.active_segment()).unwrap();
    let schema_records = frame_starts(&segment)
        .into_iter()
        .filter(|start| segment[start + 12 + 8] == 5)
        .count();
    assert_eq!(schema_records, 2);
    // The segment marker still fences older releases; the schema commit
    // does not rewrite it as a generation state.
    let state: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .0
                .join(".fylo-transactions/.collections/items/state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state["format"], "fylo.collection-segments.v1");
    assert_ne!(state["format"], GenerationState::FORMAT_V3);
}

#[test]
fn a_version_that_does_not_follow_the_records_head_is_refused() {
    let fixture = Fixture::segments("ordinal");
    assert_eq!(
        fixture.commit(2).unwrap_err().code(),
        NativeStorageErrorCode::ConcurrentWrite
    );
    fixture.commit(1).unwrap();
    assert_eq!(
        fixture.commit(1).unwrap_err().code(),
        NativeStorageErrorCode::ConcurrentWrite
    );
    assert_eq!(fixture.head(), Some(1));
}

#[test]
fn a_refused_verification_appends_nothing() {
    let fixture = Fixture::segments("verify");
    let before = fs::read(fixture.active_segment()).unwrap();
    let error = fixture
        .writer()
        .commit_schema_version("items", 1, b"{}", |_| {
            Err(NativeStorageError::codec_rejected("dry run failed"))
        })
        .unwrap_err();
    assert!(error.to_string().contains("dry run failed"), "{error}");
    assert_eq!(fixture.head(), None);
    let after = fs::read(fixture.active_segment()).unwrap();
    assert!(
        !frame_starts(&after)
            .into_iter()
            .any(|start| after[start + 12 + 8] == 5),
        "a refused version left a schema record"
    );
    assert!(after.len() >= before.len());
    assert!(fixture.descriptor().get("schemaHead").is_none());
}

#[test]
fn recovery_replays_a_schema_record_the_snapshot_does_not_hold() {
    let fixture = Fixture::segments("replay");
    fixture.commit(1).unwrap();
    let snapshot_at_one = fs::read(fixture.snapshot_path()).unwrap();
    fixture.put(1, &json!({ "name": "Ada" }));
    fixture.commit(2).unwrap();
    // Recreate a crash between the v2 record's flush and its snapshot: cut
    // the checkpoint that closed the commit, put the v1 snapshot and
    // descriptor head back, and lose the slots.
    let segment = fixture.active_segment();
    let bytes = fs::read(&segment).unwrap();
    let starts = frame_starts(&bytes);
    let last = *starts.last().unwrap();
    assert_eq!(bytes[last + 12 + 8], 3, "the commit ends with a checkpoint");
    assert_eq!(
        bytes[starts[starts.len() - 2] + 12 + 8],
        5,
        "the v2 record precedes it"
    );
    fs::write(&segment, &bytes[..last]).unwrap();
    fs::write(fixture.snapshot_path(), &snapshot_at_one).unwrap();
    fixture.set_descriptor_head(1);
    fixture.mark_writing();

    let writer = fixture.writer();
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fixture.head(), Some(2));
    assert_eq!(fixture.read(2).unwrap(), version(2).as_bytes());
    assert_eq!(fixture.descriptor()["schemaHead"], 2);
    assert!(!writer.recover_collection("items").unwrap());
    assert!(writer.verify_segments("items").unwrap().defects.is_empty());
    // The collection keeps working at the new head.
    fixture.commit(3).unwrap();
    assert_eq!(fixture.head(), Some(3));
}

#[test]
fn a_torn_schema_record_leaves_the_previous_head() {
    let fixture = Fixture::segments("torn");
    fixture.commit(1).unwrap();
    let snapshot_at_one = fs::read(fixture.snapshot_path()).unwrap();
    fixture.commit(2).unwrap();
    let segment = fixture.active_segment();
    let bytes = fs::read(&segment).unwrap();
    let starts = frame_starts(&bytes);
    let record = starts[starts.len() - 2];
    // Half of the v2 record reached the disk.
    let torn = record + (starts[starts.len() - 1] - record) / 2;
    fs::write(&segment, &bytes[..torn]).unwrap();
    fs::write(fixture.snapshot_path(), &snapshot_at_one).unwrap();
    fixture.set_descriptor_head(1);
    fixture.mark_writing();
    let writer = fixture.writer();
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fixture.head(), Some(1));
    assert_eq!(fixture.descriptor()["schemaHead"], 1);
    fixture.commit(2).unwrap();
    assert_eq!(fixture.head(), Some(2));
}

#[test]
fn a_deleted_or_damaged_snapshot_is_rebuilt_from_the_records() {
    let fixture = Fixture::segments("rebuild");
    fixture.commit(1).unwrap();
    fixture.commit(2).unwrap();
    let good = fs::read(fixture.snapshot_path()).unwrap();
    let writer = fixture.writer();

    fs::remove_file(fixture.snapshot_path()).unwrap();
    assert_eq!(fixture.head(), Some(2));
    assert_eq!(fixture.read(2).unwrap(), version(2).as_bytes());
    let verification = writer.verify_segments("items").unwrap();
    assert!(
        verification
            .defects
            .iter()
            .any(|defect| defect.contains("schemas snapshot")),
        "{verification:?}"
    );
    // A writer's next checkpoint writes it again.
    writer.rebuild_collection("items").unwrap();
    assert_eq!(fs::read(fixture.snapshot_path()).unwrap(), good);

    fs::write(fixture.snapshot_path(), b"{\"format\":\"garbage\"}").unwrap();
    assert_eq!(fixture.head(), Some(2));
    fixture.mark_writing();
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fs::read(fixture.snapshot_path()).unwrap(), good);
    assert!(writer.verify_segments("items").unwrap().defects.is_empty());
}

#[test]
fn a_snapshot_older_than_the_descriptor_is_not_trusted() {
    let fixture = Fixture::segments("stale");
    fixture.commit(1).unwrap();
    let snapshot_at_one = fs::read(fixture.snapshot_path()).unwrap();
    fixture.commit(2).unwrap();
    fs::write(fixture.snapshot_path(), &snapshot_at_one).unwrap();
    // The descriptor names version 2, so the stale snapshot is rebuilt.
    assert_eq!(fixture.head(), Some(2));
    // A writer's next commit works from the records too.
    fixture.commit(3).unwrap();
    assert_eq!(fixture.head(), Some(3));
    assert_eq!(fixture.snapshot()["versions"].as_array().unwrap().len(), 3);
}

#[test]
fn a_descriptor_ahead_of_the_records_fails_closed() {
    let fixture = Fixture::segments("ahead");
    fixture.commit(1).unwrap();
    fixture.set_descriptor_head(4);
    let error = fixture.root().schema_registry_head("items").unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::CorruptMetadata);
    assert!(fixture.commit(2).is_err());
}

#[test]
fn compaction_keeps_schema_records() {
    let fixture = Fixture::segments("compact");
    fixture.commit(1).unwrap();
    for number in 0..30 {
        fixture.put(number, &json!({ "name": format!("user{number}") }));
    }
    fixture.commit(2).unwrap();
    let writer = fixture.writer();
    for round in 0..3 {
        for number in 0..30 {
            writer
                .patch_document(
                    "items",
                    &identifier(number),
                    json!({ "round": round }).to_string().as_bytes(),
                    None,
                )
                .unwrap();
        }
    }
    let before: Vec<PathBuf> = fixture.segment_files();
    let report = writer.compact_collection("items").unwrap();
    assert!(report.segments_compacted >= 1, "{report:?}");
    let after = fixture.segment_files();
    assert!(
        !after.contains(&before[0]),
        "the first segment, holding v1, was compacted away: {after:?}"
    );
    assert_eq!(fixture.head(), Some(2));
    assert_eq!(fixture.read(1).unwrap(), version(1).as_bytes());
    assert_eq!(fixture.read(2).unwrap(), version(2).as_bytes());
    let listed: Vec<u64> = fixture.snapshot()["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["segment"].as_u64().unwrap())
        .collect();
    for segment in &listed {
        assert!(
            after
                .iter()
                .any(|path| path.ends_with(format!("{segment:010}.seg"))),
            "snapshot names segment {segment}, which is gone: {after:?}"
        );
    }
    let verification = writer.verify_segments("items").unwrap();
    assert!(verification.defects.is_empty(), "{verification:?}");
    assert_eq!(verification.schema_versions, 2);
    // A rebuild from the records and a recovery agree, and a second
    // compaction keeps the copies.
    writer.rebuild_collection("items").unwrap();
    fixture.mark_writing();
    assert!(writer.recover_collection("items").unwrap());
    writer.compact_collection("items").unwrap();
    assert_eq!(fixture.head(), Some(2));
    assert_eq!(fixture.read(1).unwrap(), version(1).as_bytes());
    fixture.commit(3).unwrap();
    assert_eq!(fixture.head(), Some(3));
}

#[test]
fn salvage_writes_the_versions_as_registry_files() {
    let fixture = Fixture::segments("salvage");
    fixture.commit(1).unwrap();
    fixture.put(1, &json!({ "name": "Ada" }));
    fixture.commit(2).unwrap();
    let out = scratch("salvage-out");
    let report = fixture.writer().salvage_collection("items", &out).unwrap();
    assert_eq!(report.schema_versions, vec![1, 2]);
    assert_eq!(report.recovered, 1);
    for ordinal in [1, 2] {
        assert_eq!(
            fs::read(out.join(format!(".fylo-schemas/{ordinal}.json"))).unwrap(),
            version(ordinal).as_bytes()
        );
    }
    let _ = fs::remove_dir_all(out);
}

#[test]
fn verify_reports_a_damaged_schema_record() {
    let fixture = Fixture::segments("damage");
    fixture.commit(1).unwrap();
    fixture.put(1, &json!({ "name": "Ada" }));
    let segment = fixture.active_segment();
    let mut bytes = fs::read(&segment).unwrap();
    let needle = b"\"label\": \"v1\"";
    let at = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    bytes[at + 11] ^= 0x01;
    fs::write(&segment, &bytes).unwrap();
    let verification = fixture.writer().verify_segments("items").unwrap();
    assert!(!verification.defects.is_empty(), "{verification:?}");
    let out = scratch("damage-out");
    let report = fixture.writer().salvage_collection("items", &out).unwrap();
    assert!(report.schema_versions.is_empty(), "{report:?}");
    assert_eq!(report.recovered, 1);
    let _ = fs::remove_dir_all(out);
}

#[test]
fn layout_migration_carries_the_registry_both_ways() {
    let fixture = Fixture::create("migrate", CollectionLayout::Files);
    let files = fixture.writer_with(CollectionLayout::Files);
    for ordinal in [1, 2] {
        files
            .commit_schema_version("items", ordinal, version(ordinal).as_bytes(), |_| Ok(()))
            .unwrap();
    }
    fixture.put(1, &json!({ "name": "Ada" }));
    NativeWriteRoot::close_logs_under(&fixture.0).unwrap();

    files
        .migrate_layout("items", CollectionLayout::Segments, false)
        .unwrap();
    assert_eq!(fixture.head(), Some(2));
    assert_eq!(fixture.snapshot()["versions"].as_array().unwrap().len(), 2);
    for ordinal in [1, 2] {
        assert_eq!(fixture.read(ordinal).unwrap(), version(ordinal).as_bytes());
    }
    fixture.commit(3).unwrap();
    assert!(
        fixture
            .writer()
            .verify_segments("items")
            .unwrap()
            .defects
            .is_empty()
    );

    files
        .migrate_layout("items", CollectionLayout::Files, false)
        .unwrap();
    assert_eq!(fixture.head(), Some(3));
    assert_eq!(
        fs::read(fixture.0.join(".fylo-schemas/items/3.json")).unwrap(),
        version(3).as_bytes()
    );
    let state: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .0
                .join(".fylo-transactions/.collections/items/state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state["format"], GenerationState::FORMAT_V3, "{state}");
    files
        .commit_schema_version("items", 4, version(4).as_bytes(), |_| Ok(()))
        .unwrap();
    assert_eq!(fixture.head(), Some(4));
}

#[test]
fn a_dropped_segment_collection_starts_again_without_a_registry() {
    let fixture = Fixture::segments("drop");
    fixture.commit(1).unwrap();
    let writer = fixture.writer();
    writer.drop_collection("items").unwrap();
    writer
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    assert_eq!(fixture.head(), None);
    fixture.commit(1).unwrap();
    assert_eq!(fixture.head(), Some(1));
}
