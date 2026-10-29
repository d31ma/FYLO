//! A `.fxdb` database root over the machine protocol (ADR 0016 section 11):
//! a session admits it without a root lease, creates it on first use, and
//! writes, reads and queries its document collections like a directory
//! root's. Features a database does not hold are refused by name.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_machine::{FrameLimits, serve_configured};
use fylo_storage_native::{NativeRoot, RootConfig};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "fylo-machine-fxdb-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&directory).unwrap();
        Self(fs::canonicalize(directory).unwrap())
    }

    fn database(&self) -> PathBuf {
        self.0.join("app.fxdb")
    }

    /// Answer `requests` in one session, each naming the database as its
    /// root.
    fn session(&self, requests: &[Value]) -> Vec<Value> {
        let root = self.database();
        let mut input = String::new();
        for request in requests {
            let mut request = request.clone();
            request["root"] = json!(root);
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
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn results(&self, requests: &[Value]) -> Vec<Value> {
        self.session(requests)
            .into_iter()
            .zip(requests)
            .map(|(reply, request)| {
                assert_eq!(reply["ok"], json!(true), "{request} failed: {reply}");
                reply["result"].clone()
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.database());
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(number: usize) -> String {
    format!("4VRNF5{:05}", 40_000 + number)
}

#[test]
fn a_session_creates_writes_and_queries_a_database() {
    let fixture = Fixture::new();
    let mut requests = vec![json!({ "op": "createCollection", "collection": "users" })];
    for number in 0..6 {
        let team = ["red", "blue"][number % 2];
        requests.push(json!({
            "op": "putData", "collection": "users", "id": id(number),
            "data": { "name": format!("user{number}"), "team": team }
        }));
    }
    fixture.results(&requests);
    // Nothing but the database (and its lease) was created beside it.
    assert!(fixture.database().is_file());
    assert!(!fixture.0.join(".fylo-data").exists());
    assert!(!fixture.0.join(".fylo-root.lock").exists());

    // A later session reads what the first wrote.
    NativeRoot::close_database(fixture.database()).unwrap();
    let results = fixture.results(&[
        json!({ "op": "getDoc", "collection": "users", "id": id(3) }),
        json!({
            "op": "findDocs", "collection": "users",
            "query": { "$ops": [{ "team": { "$eq": "red" } }] }
        }),
        json!({ "op": "delDoc", "collection": "users", "id": id(0) }),
        json!({ "op": "findDocs", "collection": "users", "query": {} }),
        json!({ "op": "executeSQL", "sql": "SELECT * FROM users WHERE team = 'blue'" }),
        json!({ "op": "executeSQL", "sql": "UPDATE users SET team = 'green' WHERE team = 'blue'" }),
        json!({
            "op": "findDocs", "collection": "users",
            "query": { "$ops": [{ "team": { "$eq": "green" } }] }
        }),
    ]);
    assert_eq!(results[4].as_object().unwrap().len(), 3);
    assert_eq!(results[6].as_object().unwrap().len(), 3);
    assert_eq!(results[0][id(3)]["name"], json!("user3"));
    let mut red: Vec<&String> = results[1].as_object().unwrap().keys().collect();
    red.sort();
    assert_eq!(red, [&id(0), &id(2), &id(4)]);
    assert_eq!(results[3].as_object().unwrap().len(), 5);
}

#[test]
fn a_database_refuses_what_it_does_not_hold() {
    let fixture = Fixture::new();
    let replies = fixture.session(&[
        json!({ "op": "createCollection", "collection": "blobs", "kind": "file" }),
        json!({ "op": "queuePublish", "queue": "jobs", "payload": {} }),
    ]);
    for reply in &replies {
        assert_eq!(reply["ok"], json!(false), "{reply}");
    }
    assert_eq!(replies[0]["error"]["code"], json!("ELAYOUTUNSUPPORTED"));
}
