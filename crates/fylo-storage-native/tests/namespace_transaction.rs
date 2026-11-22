use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;

use fylo_storage_native::{
    CollectionKind, CollectionWrites, DocumentWrite, GenerationStatus, NativeRoot,
    NativeStorageErrorCode, NativeWriteRoot, PlainDocumentCodec, RootLease, RootOpenOptions,
};
use serde_json::{Map, Value, json};

const CHILD: &str = "FYLO_TXN2_CHILD";
const NAMES: [&str; 3] = ["alpha", "beta", "gamma"];

struct Fixture(PathBuf);

impl Fixture {
    fn create(label: &str) -> Self {
        let path = fylo_test_support::TestFolder::path().join(format!(
            "fylo-txn2-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir(&path).unwrap();
        let writer = NativeWriteRoot::open(&path).unwrap();
        for name in NAMES {
            writer
                .create_collection(name, CollectionKind::Document, None)
                .unwrap();
        }
        writer
            .create_collection("files", CollectionKind::File, None)
            .unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(&self.0).unwrap()
    }

    fn transactions(&self, collection: &str) -> PathBuf {
        self.0
            .join(".fylo-transactions/.collections")
            .join(collection)
    }

    fn namespace_log(&self) -> PathBuf {
        self.0.join(".fylo-transactions/.namespace/log/segment.log")
    }

    fn state(&self, collection: &str) -> Value {
        serde_json::from_slice(&fs::read(self.transactions(collection).join("state.json")).unwrap())
            .unwrap()
    }

    fn body(&self, collection: &str, identifier: &str) -> Option<Value> {
        let stored = NativeRoot::open(&self.0)
            .unwrap()
            .collection(collection)
            .unwrap()
            .read_document(identifier)
            .ok()?;
        Some(serde_json::from_slice(&stored.bytes).unwrap())
    }

    fn count(&self, collection: &str) -> usize {
        NativeRoot::open(&self.0)
            .unwrap()
            .collection(collection)
            .unwrap()
            .document_ids()
            .unwrap()
            .len()
    }

    fn index_matches_rebuild(&self, collection: &str) {
        let root = NativeRoot::open(&self.0).unwrap();
        let before = root
            .collection(collection)
            .unwrap()
            .index_snapshot()
            .unwrap()
            .as_bytes()
            .to_vec();
        self.writer().rebuild_collection(collection).unwrap();
        let rebuilt = root
            .collection(collection)
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
    format!("4VRNF5{:05}", 30_000 + number)
}

fn fields(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn put(number: usize, label: &str) -> DocumentWrite {
    DocumentWrite::Put {
        identifier: identifier(number),
        fields: fields(&json!({ "label": label, "number": number })),
    }
}

fn replace(number: usize, label: &str) -> DocumentWrite {
    DocumentWrite::Replace {
        identifier: identifier(number),
        fields: fields(&json!({ "label": label, "number": number })),
    }
}

fn batch(collection: &str, writes: Vec<DocumentWrite>) -> CollectionWrites {
    CollectionWrites {
        collection: collection.to_owned(),
        writes,
    }
}

fn commit(writer: &NativeWriteRoot, batches: &[CollectionWrites]) {
    writer
        .commit_collection_writes_with(batches, &PlainDocumentCodec)
        .unwrap();
}

fn two_collection_batches(first: usize, label: &str) -> Vec<CollectionWrites> {
    vec![
        batch("alpha", vec![put(first, label)]),
        batch("beta", vec![put(first + 1, label), put(first + 2, label)]),
    ]
}

#[test]
fn two_collections_commit_together_and_a_close_leaves_v1() {
    let fixture = Fixture::create("two");
    let writer = fixture.writer();
    commit(&writer, &two_collection_batches(1, "first"));
    assert_eq!(
        fixture.body("alpha", &identifier(1)).unwrap()["label"],
        "first"
    );
    assert_eq!(fixture.count("beta"), 2);
    assert!(fixture.namespace_log().is_file());
    for name in ["alpha", "beta"] {
        assert_eq!(fixture.state(name)["state"], "stable");
        assert_eq!(
            fixture.state(name)["format"],
            "fylo.collection-generation.v2"
        );
    }
    NativeWriteRoot::close_logs_under(writer.path()).unwrap();
    assert!(!fixture.namespace_log().exists());
    for name in ["alpha", "beta"] {
        assert_eq!(
            fixture.state(name)["format"],
            "fylo.collection-generation.v1"
        );
        fixture.index_matches_rebuild(name);
    }
}

#[test]
fn three_collections_commit_together() {
    let fixture = Fixture::create("three");
    let writer = fixture.writer();
    let mut batches = two_collection_batches(10, "three");
    batches.push(batch("gamma", vec![put(13, "three")]));
    commit(&writer, &batches);
    assert_eq!(fixture.count("alpha"), 1);
    assert_eq!(fixture.count("beta"), 2);
    assert_eq!(fixture.count("gamma"), 1);
    commit(
        &writer,
        &[
            batch("gamma", vec![replace(13, "again")]),
            batch(
                "alpha",
                vec![DocumentWrite::Delete {
                    identifier: identifier(10),
                }],
            ),
        ],
    );
    assert_eq!(
        fixture.body("gamma", &identifier(13)).unwrap()["label"],
        "again"
    );
    assert!(fixture.body("alpha", &identifier(10)).is_none());
    for name in NAMES {
        fixture.index_matches_rebuild(name);
    }
}

#[test]
fn a_failing_write_in_one_collection_writes_nothing_anywhere() {
    let fixture = Fixture::create("failing");
    let writer = fixture.writer();
    let error = writer
        .commit_collection_writes_with(
            &[
                batch("alpha", vec![put(20, "lost")]),
                batch("beta", vec![replace(21, "missing")]),
            ],
            &PlainDocumentCodec,
        )
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::NotFound);
    assert_eq!(fixture.count("alpha"), 0);
    assert!(!fixture.namespace_log().exists());
}

#[test]
fn a_raw_file_collection_is_refused() {
    let fixture = Fixture::create("raw");
    let error = fixture
        .writer()
        .commit_collection_writes_with(
            &[
                batch("alpha", vec![put(30, "raw")]),
                batch("files", vec![put(31, "raw")]),
            ],
            &PlainDocumentCodec,
        )
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
    assert_eq!(fixture.count("alpha"), 0);
}

#[test]
fn a_collection_named_twice_is_refused() {
    let fixture = Fixture::create("twice");
    let error = fixture
        .writer()
        .commit_collection_writes_with(
            &[
                batch("alpha", vec![put(32, "twice")]),
                batch("alpha", vec![put(33, "twice")]),
            ],
            &PlainDocumentCodec,
        )
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::InvalidCollection);
    assert_eq!(fixture.count("alpha"), 0);
}

#[test]
fn recovery_applies_a_part_whose_collection_log_lost_it() {
    let fixture = Fixture::create("lost-part");
    let writer = fixture.writer();
    commit(&writer, &two_collection_batches(40, "durable"));
    let beta = fixture.transactions("beta");
    let documents = NativeRoot::open(&fixture.0)
        .unwrap()
        .collection("beta")
        .unwrap()
        .document_ids()
        .unwrap();
    for id in &documents {
        let path = NativeRoot::open(&fixture.0)
            .unwrap()
            .collection("beta")
            .unwrap()
            .read_document(id)
            .unwrap()
            .path;
        fs::remove_file(path).unwrap();
    }
    fs::remove_dir_all(beta.join("log")).unwrap();
    fs::write(
        beta.join("state.json"),
        br#"{"format":"fylo.collection-generation.v1","generation":0,"state":"stable"}"#,
    )
    .unwrap();
    drop(writer);
    let generation = NativeRoot::open(&fixture.0)
        .unwrap()
        .collection("beta")
        .unwrap()
        .generation()
        .unwrap();
    assert_eq!(generation.state, GenerationStatus::Writing);
    let reopened = fixture.writer();
    assert!(reopened.recover_namespace().unwrap());
    assert_eq!(fixture.count("beta"), 2);
    assert!(!fixture.namespace_log().exists());
    assert!(!reopened.recover_namespace().unwrap());
    fixture.index_matches_rebuild("beta");
}

#[test]
fn overlapping_transactions_never_deadlock() {
    let fixture = Fixture::create("overlap");
    let root = Arc::new(fixture.0.clone());
    let pairs = [("alpha", "beta"), ("beta", "gamma"), ("gamma", "alpha")];
    let workers: Vec<_> = pairs
        .iter()
        .enumerate()
        .map(|(worker, (first, second))| {
            let root = Arc::clone(&root);
            let (first, second) = ((*first).to_owned(), (*second).to_owned());
            thread::spawn(move || {
                let writer = NativeWriteRoot::open(Path::new(&*root)).unwrap();
                for step in 0..40 {
                    let number = 1_000 + worker * 100 + step;
                    commit(
                        &writer,
                        &[
                            batch(&first, vec![put(number, "pair")]),
                            batch(&second, vec![put(number, "pair")]),
                        ],
                    );
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    for name in NAMES {
        assert_eq!(fixture.count(name), 80);
        fixture.index_matches_rebuild(name);
    }
}

fn run_crash_child(root: &Path, failpoint: &str, skip: u64) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, root)
        .env("FYLO_RUST_FAILPOINT", failpoint)
        .env("FYLO_RUST_FAILPOINT_SKIP", skip.to_string())
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .status()
        .unwrap();
    assert!(!status.success(), "the child did not stop at {failpoint}");
}

#[test]
fn crash_child() {
    let Ok(root) = std::env::var(CHILD) else {
        return;
    };
    let writer = NativeWriteRoot::open(&root).unwrap();
    if std::env::var(POINTER_CHILD).is_ok() {
        commit(&writer, &pointer_batches(70));
        return;
    }
    let mut batches = two_collection_batches(50, "crash");
    batches.push(batch("gamma", vec![put(53, "crash")]));
    commit(&writer, &batches);
}

const POINTER_CHILD: &str = "FYLO_TXN2_POINTER";

fn pointer_batches(number: usize) -> Vec<CollectionWrites> {
    let pointer = DocumentWrite::Put {
        identifier: identifier(number),
        fields: fields(&json!({ "target": identifier(number + 1) })),
    };
    vec![
        batch("gamma", vec![put(number + 1, "target")]),
        batch("alpha", vec![pointer]),
    ]
}

#[test]
fn parts_are_applied_in_the_order_of_their_first_write() {
    let fixture = Fixture::create("apply-order");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, &fixture.0)
        .env(POINTER_CHILD, "1")
        .env("FYLO_RUST_FAILPOINT", "after-namespace-apply-part")
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .status()
        .unwrap();
    assert!(!status.success());
    let files = |collection: &str| {
        NativeRoot::open(&fixture.0)
            .unwrap()
            .collection(collection)
            .unwrap()
            .document_ids()
            .unwrap()
            .len()
    };
    assert_eq!(files("gamma"), 1);
    assert_eq!(files("alpha"), 0);
    fixture.writer().recover_namespace().unwrap();
    assert_eq!(fixture.count("alpha"), 1);
}

fn all_or_nothing_after(failpoint: &str, skip: u64, expect_committed: bool) {
    let fixture = Fixture::create(failpoint);
    run_crash_child(&fixture.0, failpoint, skip);
    let writer = fixture.writer();
    writer.recover_namespace().unwrap();
    for name in NAMES {
        writer.recover_collection(name).unwrap();
    }
    let expected = if expect_committed {
        [1, 2, 1]
    } else {
        [0, 0, 0]
    };
    for (name, count) in NAMES.iter().zip(expected) {
        assert_eq!(fixture.count(name), count, "{name} after {failpoint}");
        fixture.index_matches_rebuild(name);
    }
    assert!(!writer.recover_namespace().unwrap());
}

#[test]
fn a_crash_before_the_namespace_append_writes_nothing() {
    all_or_nothing_after("before-log-append", 0, false);
}

#[test]
fn a_crash_after_the_namespace_append_commits_every_collection() {
    all_or_nothing_after("before-namespace-log-sync", 0, true);
}

#[test]
fn a_crash_after_the_namespace_flush_commits_every_collection() {
    all_or_nothing_after("after-namespace-log-sync", 0, true);
}

#[test]
fn a_crash_between_two_applied_collections_commits_every_collection() {
    all_or_nothing_after("after-namespace-apply-part", 0, true);
    all_or_nothing_after("after-namespace-apply-part", 1, true);
}

#[test]
fn a_crash_before_the_collection_copies_commits_every_collection() {
    all_or_nothing_after("before-namespace-collection-append", 0, true);
}

#[test]
fn a_checkpoint_mark_stops_an_old_part_from_returning() {
    let fixture = Fixture::create("mark");
    let writer = fixture.writer();
    commit(
        &writer,
        &[
            batch("alpha", vec![put(60, "old")]),
            batch("beta", vec![put(61, "old")]),
        ],
    );
    commit(&writer, &[batch("alpha", vec![replace(60, "new")])]);
    writer.rebuild_collection("alpha").unwrap();
    commit(&writer, &[batch("alpha", vec![put(62, "later")])]);
    let alpha = fixture.transactions("alpha");
    assert_eq!(
        fixture.state("alpha")["format"],
        "fylo.collection-generation.v2"
    );
    fs::remove_dir_all(alpha.join("log")).unwrap();
    assert!(writer.recover_namespace().unwrap());
    assert_eq!(
        fixture.body("alpha", &identifier(60)).unwrap()["label"],
        "new"
    );
    assert_eq!(fixture.count("beta"), 1);
}

fn pointer_target(fixture_root: &Path, pointer: &str) -> Option<String> {
    let stored = NativeRoot::open(fixture_root)
        .ok()?
        .collection("alpha")
        .ok()?
        .read_document(pointer)
        .ok()?;
    let body: Value = serde_json::from_slice(&stored.bytes).ok()?;
    body["target"].as_str().map(str::to_owned)
}

fn target_state(fixture_root: &Path, target: &str) -> Result<(), NativeStorageErrorCode> {
    NativeRoot::open(fixture_root)
        .map_err(|error| error.code())?
        .collection("gamma")
        .map_err(|error| error.code())?
        .read_document(target)
        .map(drop)
        .map_err(|error| error.code())
}

#[test]
fn a_reader_that_follows_the_last_write_never_finds_its_target_missing() {
    let fixture = Fixture::create("pointer");
    let root = Arc::new(fixture.0.clone());
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (root, done) = (Arc::clone(&root), Arc::clone(&done));
        thread::spawn(move || {
            let mut followed = 0;
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let Ok(ids) = NativeRoot::open(&*root)
                    .and_then(|opened| opened.collection("alpha"))
                    .and_then(|alpha| alpha.document_ids())
                else {
                    continue;
                };
                for pointer in ids {
                    let Some(target) = pointer_target(&root, &pointer) else {
                        continue;
                    };
                    let found = target_state(&root, &target);
                    assert_ne!(found, Err(NativeStorageErrorCode::NotFound), "{target}");
                    followed += 1;
                }
            }
            followed
        })
    };
    let writer = fixture.writer();
    for step in 0..400 {
        let pointer = DocumentWrite::Put {
            identifier: identifier(2_000 + step),
            fields: fields(&json!({ "target": identifier(3_000 + step) })),
        };
        commit(
            &writer,
            &[
                batch("gamma", vec![put(3_000 + step, "target")]),
                batch("alpha", vec![pointer]),
            ],
        );
    }
    done.store(true, std::sync::atomic::Ordering::Release);
    assert!(reader.join().unwrap() > 0);
}

#[test]
fn a_writer_that_lost_its_lease_never_commits() {
    let fixture = Fixture::create("fence");
    let lease = RootLease::acquire(&fixture.0).unwrap();
    let writer =
        NativeWriteRoot::open_profiled(RootOpenOptions::new(&fixture.0).with_lease(lease)).unwrap();
    commit(&writer, &two_collection_batches(80, "owned"));
    let name = fixture.0.file_name().unwrap().to_str().unwrap().to_owned();
    let record_path = fixture
        .0
        .parent()
        .unwrap()
        .join(format!(".{name}.fylo-root-owner.lock.json"));
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    record["ownerId"] = json!("0123456789abcdef0123456789abcdef");
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    let error = writer
        .commit_collection_writes_with(&two_collection_batches(90, "lost"), &PlainDocumentCodec)
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::RootLeaseLost);
    assert!(fixture.body("alpha", &identifier(90)).is_none());
    assert!(fixture.body("beta", &identifier(91)).is_none());
}

#[test]
fn a_long_namespace_log_is_settled_by_the_writer() {
    let fixture = Fixture::create("settle");
    let writer = fixture.writer();
    for step in 0..4_100 {
        commit(
            &writer,
            &[
                batch("alpha", vec![put(10_000 + step, "many")]),
                batch("beta", vec![put(10_000 + step, "many")]),
            ],
        );
    }
    let records = fs::metadata(fixture.namespace_log()).map_or(0, |metadata| metadata.len());
    assert!(
        records < 4 * 1024 * 1024,
        "namespace log was not settled: {records} bytes"
    );
    assert_eq!(fixture.count("alpha"), 4_100);
    fixture.index_matches_rebuild("alpha");
}
