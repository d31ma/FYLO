use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use fylo_storage_native::{CollectionKind, NativeRoot, NativeWriteRoot, PutDocumentOptions};
use serde_json::{Value, json};

struct Fixture(PathBuf);

impl Fixture {
    fn create(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-wal-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        let writer = NativeWriteRoot::open(&path).unwrap();
        writer
            .create_collection("items", CollectionKind::Document, None)
            .unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.transactions().join("state.json")).unwrap()).unwrap()
    }

    fn transactions(&self) -> PathBuf {
        self.0.join(".fylo-transactions/.collections/items")
    }

    fn segment(&self) -> PathBuf {
        self.transactions().join("log/segment.log")
    }

    fn document(&self, identifier: &str) -> PathBuf {
        NativeRoot::open(&self.0)
            .unwrap()
            .collection("items")
            .unwrap()
            .read_document(identifier)
            .unwrap()
            .path
    }

    fn live(&self, identifier: &str) -> bool {
        NativeRoot::open(&self.0)
            .unwrap()
            .collection("items")
            .unwrap()
            .read_document(identifier)
            .is_ok()
    }

    fn index_matches_rebuild(&self) {
        let root = NativeRoot::open(&self.0).unwrap();
        let before = root
            .collection("items")
            .unwrap()
            .index_snapshot()
            .unwrap()
            .as_bytes()
            .to_vec();
        NativeWriteRoot::open(&self.0)
            .unwrap()
            .rebuild_collection("items")
            .unwrap();
        let rebuilt = root
            .collection("items")
            .unwrap()
            .index_snapshot()
            .unwrap()
            .as_bytes()
            .to_vec();
        assert_eq!(sorted_lines(&before), sorted_lines(&rebuilt));
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

fn mark_writing(fixture: &Fixture) {
    let mut state = fixture.state();
    state["state"] = json!("writing");
    state["transactionId"] = json!("log-crash");
    fs::write(
        fixture.transactions().join("state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
}

#[test]
fn a_commit_leaves_a_v2_state_and_a_checkpoint_returns_to_v1() {
    let fixture = Fixture::create("checkpoint");
    let writer = NativeWriteRoot::open(&fixture.0).unwrap();
    put(&writer, 1);
    assert_eq!(fixture.state()["format"], "fylo.collection-generation.v2");
    assert_eq!(fixture.state()["state"], "stable");
    assert!(fixture.segment().is_file());
    NativeWriteRoot::close_logs().unwrap();
    assert_eq!(fixture.state()["format"], "fylo.collection-generation.v1");
    assert!(!fixture.transactions().join("log").exists());
    fixture.index_matches_rebuild();
}

#[test]
fn recovery_replays_a_commit_whose_apply_was_lost() {
    let fixture = Fixture::create("replay");
    let writer = NativeWriteRoot::open(&fixture.0).unwrap();
    put(&writer, 2);
    let document = fixture.document(&identifier(2));
    let bytes = fs::read(&document).unwrap();
    fs::remove_file(&document).unwrap();
    mark_writing(&fixture);
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fs::read(&document).unwrap(), bytes);
    assert!(!writer.recover_collection("items").unwrap());
    fixture.index_matches_rebuild();
}

#[test]
fn recovery_ignores_a_torn_tail() {
    let fixture = Fixture::create("torn");
    let writer = NativeWriteRoot::open(&fixture.0).unwrap();
    put(&writer, 3);
    let mut segment = fs::read(fixture.segment()).unwrap();
    segment.extend_from_slice(&[0x40, 0, 0, 0, 1, 2, 3]);
    fs::write(fixture.segment(), segment).unwrap();
    assert!(writer.recover_collection("items").unwrap());
    assert!(fixture.live(&identifier(3)));
    put(&writer, 4);
    fixture.index_matches_rebuild();
}

#[test]
fn a_reader_treats_an_unapplied_record_as_a_write_in_progress() {
    let fixture = Fixture::create("reader");
    let writer = NativeWriteRoot::open(&fixture.0).unwrap();
    put(&writer, 5);
    let mut segment = fs::read(fixture.segment()).unwrap();
    segment.push(0);
    fs::write(fixture.segment(), segment).unwrap();
    let root = NativeRoot::open(&fixture.0).unwrap();
    let generation = root.collection("items").unwrap().generation().unwrap();
    assert_eq!(generation.state, fylo_storage_native::GenerationStatus::Writing);
}

#[test]
fn concurrent_writers_in_one_process_all_commit() {
    let fixture = Fixture::create("concurrent");
    let root = Arc::new(fixture.0.clone());
    let workers: Vec<_> = (0..8)
        .map(|worker| {
            let root = Arc::clone(&root);
            thread::spawn(move || {
                let writer = NativeWriteRoot::open(Path::new(&*root)).unwrap();
                for step in 0..25 {
                    put(&writer, worker * 100 + step);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let collection = NativeRoot::open(&fixture.0)
        .unwrap()
        .collection("items")
        .unwrap();
    assert_eq!(collection.document_ids().unwrap().len(), 200);
    collection.verify_index_references().unwrap();
    fixture.index_matches_rebuild();
}

#[test]
fn patch_delete_and_restore_round_trip_through_the_log() {
    let fixture = Fixture::create("round-trip");
    let writer = NativeWriteRoot::open(&fixture.0).unwrap();
    put(&writer, 6);
    let id = identifier(6);
    writer
        .patch_document_fields("items", &id, json!({"group": 99}).as_object().unwrap(), None)
        .unwrap();
    writer.delete_document("items", &id, None).unwrap();
    assert!(!fixture.live(&id));
    writer.restore_document("items", &id, None).unwrap();
    let stored = NativeRoot::open(&fixture.0)
        .unwrap()
        .collection("items")
        .unwrap()
        .read_document(&id)
        .unwrap();
    let value: Value = serde_json::from_slice(&stored.bytes).unwrap();
    assert_eq!(value["group"], 99);
    fixture.index_matches_rebuild();
}
