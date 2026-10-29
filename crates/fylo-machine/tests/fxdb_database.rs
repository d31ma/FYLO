//! A `.fxdb` database root over the machine protocol (ADR 0016 section 11):
//! a session admits it without a root lease, creates it on first use, and
//! writes, reads and queries its document collections like a directory
//! root's. Features a database does not hold are refused by name.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, serve_application, serve_configured};
use fylo_storage_native::{DatabaseKey, NativeRoot, RootConfig};
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
        self.session_with(None, requests)
    }

    /// Answer `requests` in one session whose configuration holds
    /// `application`'s database key, or none.
    fn session_with(
        &self,
        application: Option<&ApplicationConfig>,
        requests: &[Value],
    ) -> Vec<Value> {
        let root = self.database();
        let mut input = String::new();
        for request in requests {
            let mut request = request.clone();
            request["root"] = json!(root);
            input.push_str(&request.to_string());
            input.push('\n');
        }
        let mut output = Vec::new();
        let mut reader = Cursor::new(input.into_bytes());
        match application {
            Some(application) => serve_application(
                &mut reader,
                &mut output,
                None,
                FrameLimits::default(),
                application.clone(),
                false,
                false,
            ),
            None => serve_configured(
                &mut reader,
                &mut output,
                None,
                FrameLimits::default(),
                RootConfig::default(),
            ),
        }
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

/// A configuration with a generated database key, which creates encrypted
/// databases when `encrypt` is set.
fn keyed(secret: &str, encrypt: bool) -> ApplicationConfig {
    ApplicationConfig {
        database_key: Some(DatabaseKey::new(secret)),
        encrypt_databases: encrypt,
        ..ApplicationConfig::default()
    }
}

fn generated_secret() -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("generated-{stamp:032x}-{}", std::process::id())
}

#[test]
fn an_encrypted_database_opens_with_its_key_and_is_locked_without_it() {
    let fixture = Fixture::new();
    let secret = generated_secret();
    let creator = keyed(&secret, true);
    let results = fixture
        .session_with(
            Some(&creator),
            &[
                json!({ "op": "createCollection", "collection": "users" }),
                json!({
                    "op": "putData", "collection": "users", "id": id(1),
                    "data": { "name": "WOMBAT-CANARY", "team": "red" }
                }),
                json!({ "op": "getDoc", "collection": "users", "id": id(1) }),
                json!({ "op": "inspectDatabase" }),
            ],
        )
        .into_iter()
        .map(|reply| {
            assert_eq!(reply["ok"], json!(true), "{reply}");
            reply["result"].clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(results[2][id(1)]["name"], json!("WOMBAT-CANARY"));
    assert_eq!(results[3]["encrypted"], json!(true));
    assert_eq!(results[3]["locked"], json!(false));
    assert_eq!(results[3]["collections"][0]["name"], json!("users"));
    NativeRoot::close_database(fixture.database()).unwrap();
    let bytes = fs::read(fixture.database()).unwrap();
    assert!(
        !bytes
            .windows(b"WOMBAT-CANARY".len())
            .any(|window| window == b"WOMBAT-CANARY")
    );

    // Without the key it is locked: reads of a collection and every write
    // fail with EKEY, and inspect and verify still answer.
    let locked = fixture.session(&[
        json!({ "op": "getDoc", "collection": "users", "id": id(1) }),
        json!({
            "op": "putData", "collection": "users", "id": id(2),
            "data": { "name": "late" }
        }),
        json!({ "op": "createCollection", "collection": "teams" }),
        json!({ "op": "inspectDatabase" }),
        json!({ "op": "verifyDatabase" }),
    ]);
    for reply in &locked[..3] {
        assert_eq!(reply["ok"], json!(false), "{reply}");
        assert_eq!(reply["error"]["code"], json!("EKEY"), "{reply}");
    }
    assert_eq!(locked[3]["result"]["encrypted"], json!(true));
    assert_eq!(locked[3]["result"]["locked"], json!(true));
    assert_eq!(locked[3]["result"]["collections"], json!([]));
    assert_eq!(locked[4]["result"]["ok"], json!(true), "{}", locked[4]);
    NativeRoot::close_database(fixture.database()).unwrap();
    assert_eq!(fs::read(fixture.database()).unwrap(), bytes);

    // A wrong key is refused by name; the right one reads, without
    // creating anything.
    let wrong = keyed(&generated_secret(), false);
    for reply in fixture.session_with(
        Some(&wrong),
        &[
            json!({ "op": "getDoc", "collection": "users", "id": id(1) }),
            json!({ "op": "inspectDatabase" }),
        ],
    ) {
        assert_eq!(reply["error"]["code"], json!("EKEY"), "{reply}");
    }
    NativeRoot::close_database(fixture.database()).unwrap();
    let reader = keyed(&secret, false);
    let replies = fixture.session_with(
        Some(&reader),
        &[json!({ "op": "getDoc", "collection": "users", "id": id(1) })],
    );
    assert_eq!(
        replies[0]["result"][id(1)]["team"],
        json!("red"),
        "{}",
        replies[0]
    );
}

#[test]
fn a_key_without_encryption_file_creates_a_plain_database() {
    let fixture = Fixture::new();
    let application = keyed(&generated_secret(), false);
    let replies = fixture.session_with(
        Some(&application),
        &[
            json!({ "op": "createCollection", "collection": "users" }),
            json!({ "op": "inspectDatabase" }),
        ],
    );
    assert_eq!(
        replies[1]["result"]["encrypted"],
        json!(false),
        "{}",
        replies[1]
    );
    // Encryption is opt-in: a session without a key reads it.
    NativeRoot::close_database(fixture.database()).unwrap();
    let plain = fixture.results(&[json!({ "op": "inspectDatabase" })]);
    assert_eq!(plain[0]["collections"][0]["name"], json!("users"));
}
