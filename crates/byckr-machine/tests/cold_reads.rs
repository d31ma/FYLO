//! The first request of a fresh session reads only the files it needs (FYLO-CR1).
//!
//! On a cold page cache every distinct file a request opens is a disk read,
//! so a check that a collection exists must not read its documents. These
//! tests record the files a whole session opens, from its start to its last
//! answer, with `FileAccessLog`:
//!
//! - `inspectCollection` reads the index once and no document, whatever the
//!   collection's size, and its `indexedDocs` equals the full check's
//!   (`verify: true`) on a pending index log, a stale index, a missing index
//!   snapshot and a collection left in the middle of a write;
//! - `collectionExists` and `listCollections` read only the catalog.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use byckr_machine::{FrameLimits, serve};
use byckr_storage_native::FileAccessLog;
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

const PEOPLE: &str = "people";
const SMALL: &str = "small";
const MISSING: &str = "no-such-collection";
const PEOPLE_COUNT: usize = 120;
const SMALL_COUNT: usize = 6;
/// A document count lists the `docs` folder, each of its shard folders (at
/// most ten at the default shard width) and the `.deleted` folder.
const MOST_LISTINGS: usize = 12;

struct Fixture(PathBuf);

impl Fixture {
    /// A root shaped like a service's directory root: one large and one
    /// small collection, with a third of the large one patched once so its
    /// index has a pending log.
    fn new() -> Self {
        let fixture = Self::empty();
        let mut requests = Vec::new();
        Self::add_collection(&mut requests, PEOPLE, 0, PEOPLE_COUNT);
        Self::add_collection(&mut requests, SMALL, PEOPLE_COUNT, SMALL_COUNT);
        Self::add_patches(&mut requests, PEOPLE, PEOPLE_COUNT);
        let frames = fixture.session(&requests);
        let failed: Vec<&Value> = frames
            .iter()
            .filter(|frame| frame["ok"] != json!(true))
            .collect();
        assert!(failed.is_empty(), "{failed:?}");
        fixture
    }

    fn empty() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "fylo-machine-cold-reads-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&directory).unwrap();
        Self(fs::canonicalize(directory).unwrap())
    }

    fn add_collection(requests: &mut Vec<Value>, name: &str, first: usize, count: usize) {
        requests.push(json!({"op":"createCollection","collection":name}));
        for number in first..first + count {
            let team = ["red", "blue"][number % 2];
            requests.push(json!({
                "op":"putData","collection":name,"id":id(number),
                "data":{"name":format!("user{number}"),"team":team,
                        "email":format!("u{number}@example.com")}
            }));
        }
    }

    fn add_patches(requests: &mut Vec<Value>, name: &str, count: usize) {
        for number in (0..count).step_by(3) {
            requests.push(json!({
                "op":"patchDoc","collection":name,"id":id(number),"newDoc":{"team":"green"}
            }));
        }
    }

    fn session(&self, requests: &[Value]) -> Vec<Value> {
        let mut input = String::new();
        for request in requests {
            input.push_str(&request.to_string());
            input.push('\n');
        }
        let mut reader = Cursor::new(input.into_bytes());
        let mut output = Vec::new();
        serve(
            &mut reader,
            &mut output,
            Some(self.0.clone()),
            FrameLimits::default(),
        )
        .unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// One request in a fresh session, with every file the session opened.
    fn recorded(&self, request: Value) -> (Value, FileAccessLog) {
        FileAccessLog::start();
        let frames = self.session(&[request]);
        let log = FileAccessLog::finish();
        (frames.into_iter().next().unwrap(), log)
    }

    fn answer(&self, request: Value) -> Value {
        self.session(&[request]).into_iter().next().unwrap()
    }

    fn collection_path(&self, name: &str) -> PathBuf {
        self.0.join(".collections").join(name)
    }

    fn state_path(&self, name: &str) -> PathBuf {
        self.0
            .join(".fylo-transactions")
            .join(".collections")
            .join(name)
            .join("state.json")
    }

    fn first_document_file(&self, name: &str) -> PathBuf {
        let docs = self.collection_path(name).join("docs");
        let shard = sorted_entries(&docs).into_iter().next().unwrap();
        sorted_entries(&shard).into_iter().next().unwrap()
    }

    /// Mark the collection's generation as being written, as a writer that
    /// stopped between its intent and its commit leaves it.
    fn leave_mid_write(&self, name: &str) {
        let path = self.state_path(name);
        let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        state["state"] = json!("writing");
        fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(number: usize) -> String {
    format!("4VRNF5{:05}", 40_000 + number)
}

fn sorted_entries(folder: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    entries
}

fn inspect(collection: &str) -> Value {
    json!({"op":"inspectCollection","collection":collection})
}

fn full_inspect(collection: &str) -> Value {
    json!({"op":"inspectCollection","collection":collection,"verify":true})
}

/// The outcome both forms of `inspectCollection` must agree on: the result,
/// or the error code.
fn outcome(frame: &Value) -> Value {
    match frame["ok"] {
        Value::Bool(true) => frame["result"].clone(),
        _ => frame["error"]["code"].clone(),
    }
}

#[test]
fn first_inspect_reads_no_document_and_the_index_once() {
    let fixture = Fixture::new();
    let (frame, log) = fixture.recorded(inspect(PEOPLE));
    assert_eq!(frame["ok"], json!(true), "{frame}");
    assert_eq!(
        frame["result"]["docsStored"],
        json!(PEOPLE_COUNT),
        "{frame}"
    );
    assert_eq!(
        frame["result"]["indexedDocs"],
        json!(PEOPLE_COUNT),
        "{frame}"
    );
    assert_eq!(log.opens_inside("docs"), 0, "{log:#?}");
    assert_eq!(log.opens_ending_with("keys.snapshot"), 1, "{log:#?}");
    assert_eq!(log.opens_ending_with("keys.wal"), 1, "{log:#?}");
    assert_eq!(log.opens_ending_with("people.json"), 1, "{log:#?}");
}

#[test]
fn first_inspect_opens_as_many_files_for_a_large_collection_as_for_a_small_one() {
    let fixture = Fixture::new();
    let (large_frame, large) = fixture.recorded(inspect(PEOPLE));
    let (small_frame, small) = fixture.recorded(inspect(SMALL));
    assert_eq!(large_frame["ok"], json!(true), "{large_frame}");
    assert_eq!(small_frame["ok"], json!(true), "{small_frame}");
    assert_eq!(
        large.opened().len(),
        small.opened().len(),
        "{large:#?}\n{small:#?}"
    );
    assert!(large.listed().len() <= MOST_LISTINGS, "{large:#?}");
}

#[test]
fn full_inspect_reads_every_document_and_reports_the_same_counts() {
    let fixture = Fixture::new();
    let (full, log) = fixture.recorded(full_inspect(PEOPLE));
    assert_eq!(full["result"], fixture.answer(inspect(PEOPLE))["result"]);
    assert_eq!(log.opens_inside("docs"), PEOPLE_COUNT, "{log:#?}");
    assert_eq!(log.opens_ending_with("people.json"), 1, "{log:#?}");
    assert_eq!(log.opens_ending_with("keys.snapshot"), 1, "{log:#?}");
}

#[test]
fn inspect_of_a_missing_collection_reads_no_index() {
    let fixture = Fixture::new();
    let (frame, log) = fixture.recorded(inspect(MISSING));
    assert_eq!(frame["result"]["exists"], json!(false), "{frame}");
    assert_eq!(log.opens_inside("index"), 0, "{log:#?}");
    assert_eq!(log.opens_inside("docs"), 0, "{log:#?}");
}

#[test]
fn both_inspects_agree_on_a_pending_index_log() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .collection_path(PEOPLE)
            .join("index/keys.wal")
            .exists()
    );
    let cheap = fixture.answer(inspect(PEOPLE));
    assert_eq!(
        outcome(&cheap),
        outcome(&fixture.answer(full_inspect(PEOPLE)))
    );
}

#[test]
fn both_inspects_agree_on_an_index_that_names_a_removed_document() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.first_document_file(PEOPLE)).unwrap();
    let cheap = fixture.answer(inspect(PEOPLE));
    assert_eq!(
        outcome(&cheap),
        outcome(&fixture.answer(full_inspect(PEOPLE)))
    );
}

#[test]
fn both_inspects_agree_without_an_index_snapshot() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.collection_path(PEOPLE).join("index/keys.snapshot")).unwrap();
    let cheap = fixture.answer(inspect(PEOPLE));
    assert_eq!(
        outcome(&cheap),
        outcome(&fixture.answer(full_inspect(PEOPLE)))
    );
}

#[test]
fn both_inspects_agree_on_a_collection_left_in_the_middle_of_a_write() {
    let fixture = Fixture::new();
    fixture.leave_mid_write(PEOPLE);
    let cheap = fixture.answer(inspect(PEOPLE));
    assert_eq!(
        outcome(&cheap),
        outcome(&fixture.answer(full_inspect(PEOPLE)))
    );
}

#[test]
fn collection_exists_reads_only_the_catalog() {
    let fixture = Fixture::new();
    let (frame, log) = fixture.recorded(json!({"op":"collectionExists","collection":PEOPLE}));
    assert_eq!(
        frame["result"],
        json!({"collection":PEOPLE,"exists":true}),
        "{frame}"
    );
    assert_eq!(log.opens_inside("docs"), 0, "{log:#?}");
    assert_eq!(log.opens_inside("index"), 0, "{log:#?}");
    assert_eq!(log.opens_ending_with("state.json"), 0, "{log:#?}");
    assert!(log.listed().is_empty(), "{log:#?}");
}

#[test]
fn collection_exists_answers_false_for_a_missing_collection() {
    let fixture = Fixture::new();
    let frame = fixture.answer(json!({"op":"collectionExists","collection":MISSING}));
    assert_eq!(
        frame["result"],
        json!({"collection":MISSING,"exists":false}),
        "{frame}"
    );
}

#[test]
fn collection_exists_refuses_an_invalid_name() {
    let fixture = Fixture::new();
    let frame = fixture.answer(json!({"op":"collectionExists","collection":"../people"}));
    assert_eq!(frame["ok"], json!(false), "{frame}");
}

#[test]
fn list_collections_reads_no_document_or_index() {
    let fixture = Fixture::new();
    let (frame, log) = fixture.recorded(json!({"op":"listCollections"}));
    assert_eq!(
        frame["result"],
        json!({"collections":[PEOPLE, SMALL]}),
        "{frame}"
    );
    assert_eq!(log.opens_inside("docs"), 0, "{log:#?}");
    assert_eq!(log.opens_inside("index"), 0, "{log:#?}");
    assert_eq!(log.opens_ending_with("state.json"), 0, "{log:#?}");
}

#[test]
fn list_collections_leaves_out_a_dropped_collection() {
    let fixture = Fixture::new();
    let dropped = fixture.answer(json!({"op":"dropCollection","collection":SMALL}));
    assert_eq!(dropped["ok"], json!(true), "{dropped}");
    let frame = fixture.answer(json!({"op":"listCollections"}));
    assert_eq!(frame["result"], json!({"collections":[PEOPLE]}), "{frame}");
}
