//! Kept query match lists in an exclusive session (ADR 0030 section 3.5):
//! every insert, update and delete frame is seen by the next query, the
//! budget bounds the lists, and no list outlives its session, so a later
//! session sees what another writer did in between.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use byckr_machine::{FrameLimits, serve, serve_exclusive};
use serde_json::{Value, json};

const COLLECTION: &str = "records";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = byckr_test_support::TestFolder::path().join(format!(
            "fylo-hot-set-query-writes-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let root = parent.join("root");
        std::fs::create_dir_all(&root).unwrap();
        Self { parent, root }
    }

    /// One exclusive session that answers `frames` in order, then ends.
    fn exclusive(&self, frames: &[Value]) -> Vec<Value> {
        self.session(frames, true)
    }

    /// One session without the exclusive lease, as another process would
    /// write the root between two exclusive sessions.
    fn shared(&self, frames: &[Value]) -> Vec<Value> {
        self.session(frames, false)
    }

    fn session(&self, frames: &[Value], exclusive: bool) -> Vec<Value> {
        let mut script = String::new();
        for frame in frames {
            script.push_str(&frame.to_string());
            script.push('\n');
        }
        let mut input = Cursor::new(script.into_bytes());
        let mut output = Vec::new();
        let session = if exclusive { serve_exclusive } else { serve };
        session(
            &mut input,
            &mut output,
            Some(self.root.clone()),
            FrameLimits::default(),
        )
        .unwrap();
        let replies: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for reply in &replies {
            assert_eq!(reply["ok"], json!(true), "{reply}");
        }
        replies
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

fn id(number: usize) -> String {
    format!("4VRNF5{:05}", 30_000 + number)
}

fn create() -> Value {
    json!({ "op": "createCollection", "collection": COLLECTION })
}

fn put(number: usize, group: &str) -> Value {
    json!({ "op": "putData", "collection": COLLECTION, "id": id(number),
            "data": { "number": number, "group": group } })
}

fn patch(number: usize, group: &str) -> Value {
    json!({ "op": "patchDoc", "collection": COLLECTION, "id": id(number),
            "newDoc": { "group": group } })
}

fn delete(number: usize) -> Value {
    json!({ "op": "delDoc", "collection": COLLECTION, "id": id(number) })
}

fn find(group: &str) -> Value {
    json!({ "op": "findDocs", "collection": COLLECTION,
            "query": { "$ops": [{ "group": { "$eq": group } }] } })
}

fn handshake() -> Value {
    json!({ "op": "handshake" })
}

fn found(reply: &Value) -> Vec<String> {
    let mut ids: Vec<String> = reply["result"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    ids.sort();
    ids
}

fn hot_set(reply: &Value) -> Value {
    reply["result"]["capabilities"]["hotSet"].clone()
}

#[test]
fn a_repeated_query_is_counted_as_a_query_hit() {
    let fixture = Fixture::new();
    let replies = fixture.exclusive(&[
        create(),
        put(1, "a"),
        put(2, "a"),
        find("a"),
        find("a"),
        handshake(),
    ]);
    assert_eq!(found(&replies[3]), vec![id(1), id(2)]);
    assert_eq!(found(&replies[4]), vec![id(1), id(2)]);
    assert_eq!(hot_set(&replies[5])["queryHits"], json!(1));
    assert_eq!(hot_set(&replies[5])["queries"], json!(1));
}

#[test]
fn an_inserted_document_is_found_by_the_next_query() {
    let fixture = Fixture::new();
    let replies = fixture.exclusive(&[create(), put(1, "a"), find("a"), put(2, "a"), find("a")]);
    assert_eq!(found(&replies[2]), vec![id(1)]);
    assert_eq!(found(&replies[4]), vec![id(1), id(2)]);
}

#[test]
fn an_updated_document_is_seen_by_the_next_query() {
    let fixture = Fixture::new();
    let replies = fixture.exclusive(&[
        create(),
        put(1, "a"),
        put(2, "a"),
        find("a"),
        patch(2, "b"),
        find("a"),
        find("b"),
    ]);
    assert_eq!(found(&replies[3]), vec![id(1), id(2)]);
    assert_eq!(found(&replies[5]), vec![id(1)]);
    assert_eq!(found(&replies[6]), vec![id(2)]);
}

#[test]
fn a_deleted_document_is_gone_from_the_next_query() {
    let fixture = Fixture::new();
    let replies = fixture.exclusive(&[
        create(),
        put(1, "a"),
        put(2, "a"),
        find("a"),
        delete(1),
        find("a"),
    ]);
    assert_eq!(found(&replies[3]), vec![id(1), id(2)]);
    assert_eq!(found(&replies[5]), vec![id(2)]);
}

#[test]
fn a_new_session_sees_what_another_writer_did_after_the_last_one_ended() {
    let fixture = Fixture::new();
    let first = fixture.exclusive(&[create(), put(1, "a"), find("a"), find("a")]);
    assert_eq!(found(&first[3]), vec![id(1)]);
    fixture.shared(&[put(2, "a"), patch(1, "b")]);
    let second = fixture.exclusive(&[handshake(), find("a")]);
    assert_eq!(hot_set(&second[0])["queries"], json!(0));
    assert_eq!(found(&second[1]), vec![id(2)]);
}

#[test]
fn match_lists_stay_within_the_budget() {
    let fixture = Fixture::new();
    let mut frames = vec![create()];
    for number in 0..40 {
        frames.push(put(number, &format!("g{}", number % 8)));
    }
    for group in 0..8 {
        frames.push(find(&format!("g{group}")));
    }
    frames.push(handshake());
    let replies = fixture.exclusive(&frames);
    let statistics = hot_set(replies.last().unwrap());
    assert!(
        statistics["residentBytes"].as_u64().unwrap()
            <= statistics["budgetBytes"].as_u64().unwrap()
    );
    for group in 0..8 {
        assert_eq!(found(&replies[41 + group]).len(), 5);
    }
}
