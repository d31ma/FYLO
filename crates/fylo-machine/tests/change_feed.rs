//! The `changes` operation (ADR 0024): each document whose newest change
//! follows a durable cursor, once, oldest change first, in a `.vdbx`
//! database and in a directory collection in the segments layout.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_machine::{FrameLimits, serve_configured};
use fylo_storage_native::{NativeRoot, RootConfig};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn database() -> Self {
        let directory = Self::directory();
        let root = directory.join("app.vdbx");
        NativeRoot::create_database(&root).unwrap();
        Self { directory, root }
    }

    fn folder() -> Self {
        let directory = Self::directory();
        let root = directory.join("root");
        fs::create_dir_all(&root).unwrap();
        Self { directory, root }
    }

    fn directory() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "fylo-machine-changes-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::canonicalize(directory).unwrap()
    }

    fn session(&self, requests: &[Value]) -> Vec<Value> {
        let mut input = String::new();
        for request in requests {
            let mut request = request.clone();
            request["root"] = json!(self.root);
            input.push_str(&request.to_string());
            input.push('\n');
        }
        let mut output = Vec::new();
        serve_configured(
            &mut Cursor::new(input.into_bytes()),
            &mut output,
            None,
            FrameLimits::default(),
            RootConfig::default(),
        )
        .unwrap();
        let _ = NativeRoot::close_database(&self.root);
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn result(&self, request: &Value) -> Value {
        let reply = self.session(std::slice::from_ref(request)).remove(0);
        assert_eq!(reply["ok"], json!(true), "{request} failed: {reply}");
        reply["result"].clone()
    }

    fn error(&self, request: &Value) -> String {
        let reply = self.session(std::slice::from_ref(request)).remove(0);
        assert_eq!(reply["ok"], json!(false), "{request} succeeded: {reply}");
        reply["error"]["code"].as_str().unwrap().to_owned()
    }

    fn changes(&self, after: &Value, limit: u64) -> Value {
        self.result(
            &json!({ "op": "changes", "collection": "notes", "after": after, "limit": limit }),
        )
    }

    fn put(&self, number: usize) {
        self.result(&json!({
            "op": "putData", "collection": "notes", "id": id(number),
            "data": { "text": format!("note {number}") }
        }));
    }

    fn patch(&self, number: usize) {
        self.result(&json!({
            "op": "patchDoc", "collection": "notes", "id": id(number),
            "newDoc": { "text": format!("note {number} again") }
        }));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(&self.root);
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn id(number: usize) -> String {
    format!("4VRNF5{:05}", 40_000 + number)
}

fn listed(page: &Value) -> Vec<(String, String)> {
    page["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| {
            (
                change["id"].as_str().unwrap().to_owned(),
                change["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn put(number: usize) -> (String, String) {
    (id(number), "put".to_owned())
}

fn deleted(number: usize) -> (String, String) {
    (id(number), "delete".to_owned())
}

fn database_with_notes() -> Fixture {
    let fixture = Fixture::database();
    fixture.result(&json!({ "op": "createCollection", "collection": "notes" }));
    for number in 0..3 {
        fixture.put(number);
    }
    fixture
}

#[test]
fn pages_follow_the_cursor_across_sessions() {
    let fixture = database_with_notes();
    let first = fixture.changes(&Value::Null, 2);
    assert_eq!(listed(&first), [put(0), put(1)]);
    assert_eq!(first["more"], json!(true));
    let second = fixture.changes(&first["cursor"], 2);
    assert_eq!(listed(&second), [put(2)]);
    assert_eq!(second["more"], json!(false));
    let empty = fixture.changes(&second["cursor"], 2);
    assert_eq!(listed(&empty), Vec::<(String, String)>::new());
    assert_eq!(empty["cursor"], second["cursor"]);
}

#[test]
fn a_later_change_moves_the_document_to_the_end() {
    let fixture = database_with_notes();
    let start = fixture.changes(&Value::Null, 10);
    fixture.patch(0);
    fixture.result(&json!({ "op": "delDoc", "collection": "notes", "id": id(1) }));
    let later = fixture.changes(&start["cursor"], 10);
    assert_eq!(listed(&later), [put(0), deleted(1)]);
    let everything = fixture.changes(&Value::Null, 10);
    assert_eq!(listed(&everything), [put(2), put(0), deleted(1)]);
}

#[test]
fn a_restored_document_is_a_put_again() {
    let fixture = database_with_notes();
    fixture.result(&json!({ "op": "delDoc", "collection": "notes", "id": id(2) }));
    let deletion = fixture.changes(&Value::Null, 10);
    assert_eq!(listed(&deletion).last(), Some(&deleted(2)));
    fixture.result(&json!({ "op": "restoreDoc", "collection": "notes", "id": id(2) }));
    let restored = fixture.changes(&deletion["cursor"], 10);
    assert_eq!(listed(&restored), [put(2)]);
}

#[test]
fn one_commit_of_many_documents_is_never_cut_by_a_page() {
    let fixture = Fixture::database();
    fixture.result(&json!({ "op": "createCollection", "collection": "notes" }));
    fixture.result(&json!({
        "op": "batchPutData", "collection": "notes",
        "batch": [{ "n": 1 }, { "n": 2 }, { "n": 3 }]
    }));
    let mut cursor = Value::Null;
    let mut seen = Vec::new();
    for _ in 0..3 {
        let page = fixture.changes(&cursor, 1);
        seen.extend(listed(&page));
        cursor = page["cursor"].clone();
    }
    assert_eq!(seen.len(), 3);
    let last = fixture.changes(&cursor, 1);
    assert_eq!(listed(&last), Vec::<(String, String)>::new());
}

#[test]
fn compaction_keeps_cursors_valid() {
    let fixture = database_with_notes();
    let start = fixture.changes(&Value::Null, 10);
    fixture.patch(1);
    fixture.patch(1);
    fixture.result(&json!({ "op": "compactCollection", "collection": "notes" }));
    let later = fixture.changes(&start["cursor"], 10);
    assert_eq!(listed(&later), [put(1)]);
    let everything = fixture.changes(&Value::Null, 10);
    assert_eq!(listed(&everything), [put(0), put(2), put(1)]);
}

#[test]
fn a_recreated_collection_expires_old_cursors() {
    let fixture = database_with_notes();
    let start = fixture.changes(&Value::Null, 1);
    fixture.result(&json!({ "op": "dropCollection", "collection": "notes" }));
    fixture.result(&json!({ "op": "createCollection", "collection": "notes" }));
    for number in 0..5 {
        fixture.put(number);
    }
    let code = fixture.error(&json!({
        "op": "changes", "collection": "notes", "after": start["cursor"], "limit": 10
    }));
    assert_eq!(code, "ECURSOR_EXPIRED");
}

#[test]
fn a_cursor_past_the_collections_end_is_expired() {
    let fixture = database_with_notes();
    let start = fixture.changes(&Value::Null, 10);
    let cursor = start["cursor"].as_str().unwrap();
    let mut parts: Vec<&str> = cursor.splitn(4, '.').collect();
    parts[2] = "999999";
    let ahead = parts.join(".");
    let code = fixture.error(&json!({
        "op": "changes", "collection": "notes", "after": ahead, "limit": 10
    }));
    assert_eq!(code, "ECURSOR_EXPIRED");
}

#[test]
fn bad_cursors_and_limits_are_refused() {
    let fixture = database_with_notes();
    let invalid = fixture.error(&json!({
        "op": "changes", "collection": "notes", "after": "fqc1.7.3", "limit": 10
    }));
    assert_eq!(invalid, "EINVALIDCURSOR");
    let zero = fixture.error(&json!({ "op": "changes", "collection": "notes", "limit": 0 }));
    assert_eq!(zero, "EBADREQUEST");
    let large = fixture.error(&json!({ "op": "changes", "collection": "notes", "limit": 1001 }));
    assert_eq!(large, "EBADREQUEST");
    let typed = fixture.error(&json!({ "op": "changes", "collection": "notes", "after": 5 }));
    assert_eq!(typed, "EBADREQUEST");
}

#[test]
fn the_files_layout_is_refused_and_the_segments_layout_is_read() {
    let fixture = Fixture::folder();
    fixture.result(&json!({ "op": "createCollection", "collection": "notes" }));
    fixture.put(0);
    fixture.put(5);
    let code = fixture.error(&json!({ "op": "changes", "collection": "notes" }));
    assert_eq!(code, "ELAYOUTUNSUPPORTED");
    fixture.result(&json!({ "op": "migrateLayout", "collection": "notes", "layout": "segments" }));
    fixture.put(1);
    let page = fixture.changes(&Value::Null, 10);
    assert_eq!(listed(&page), [put(0), put(5), put(1)]);
    fixture.result(&json!({ "op": "migrateLayout", "collection": "notes", "layout": "files" }));
    let code = fixture.error(&json!({
        "op": "changes", "collection": "notes", "after": page["cursor"], "limit": 10
    }));
    assert_eq!(code, "ELAYOUTUNSUPPORTED");
}
