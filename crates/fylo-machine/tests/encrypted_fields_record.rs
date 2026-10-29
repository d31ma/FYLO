//! The encrypted-fields record: a collection whose `$encrypted` fields only a
//! `fylo.schema` folder declares keeps them encrypted for every writer. A
//! keyed writer with the folder records the head's list in the database
//! (the catalog descriptor of a directory root, the directory block of a
//! `.fxdb` database), and a writer without the folder encrypts what the
//! record lists, or is refused without the key, rather than store a declared
//! field in plaintext.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, serve_application};
use fylo_storage_native::{NativeRoot, NativeWriteRoot};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

const PII: &str = "pii";
const CY: &str = "4VRNF52JPCO";
const BO: &str = "4VRNF52JPCP";
const AL: &str = "4VRNF52JPCQ";
const KEY: &str = "fixture-key-at-least-thirty-two-bytes";

struct Fixture {
    parent: PathBuf,
    schemas: PathBuf,
}

impl Fixture {
    /// A folder declaring `pii` with `ssn` encrypted.
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-encrypted-record-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let schemas = parent.join("schemas");
        fs::create_dir_all(schemas.join(PII).join("history")).unwrap();
        fs::write(
            schemas.join(PII).join("manifest.json"),
            br#"{"current":"v1","versions":[{"v":"v1"}]}"#,
        )
        .unwrap();
        let fixture = Self {
            parent: fs::canonicalize(&parent).unwrap(),
            schemas: fs::canonicalize(schemas).unwrap(),
        };
        fixture.declare(&json!({ "$encrypted": ["ssn"], "name": "^.+$" }));
        fixture
    }

    fn declare(&self, schema: &Value) {
        fs::write(
            self.schemas.join(PII).join("history/v1.schema.json"),
            serde_json::to_vec(schema).unwrap(),
        )
        .unwrap();
    }

    fn directory(&self) -> PathBuf {
        let root = self.parent.join("root");
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn database(&self) -> PathBuf {
        self.parent.join("app.fxdb")
    }

    /// Answer `requests` against `root` in one session, with the schema
    /// folder or without it, and with the field keys or without them.
    fn session(&self, root: &Path, folder: bool, keyed: bool, requests: &[Value]) -> Vec<Value> {
        let application = ApplicationConfig {
            schema: folder.then(|| self.schemas.clone()),
            credentials: keyed.then(|| (KEY.into(), "fixture-salt".into())),
            hot_set_bytes: None,
            ..ApplicationConfig::default()
        };
        let mut input = String::new();
        for request in requests {
            let mut request = request.clone();
            request["root"] = json!(root);
            input.push_str(&request.to_string());
            input.push('\n');
        }
        let mut output = Vec::new();
        serve_application(
            &mut Cursor::new(input.into_bytes()),
            &mut output,
            None,
            FrameLimits::default(),
            application,
            false,
            false,
        )
        .unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn succeed(&self, root: &Path, folder: bool, keyed: bool, requests: &[Value]) -> Vec<Value> {
        self.session(root, folder, keyed, requests)
            .into_iter()
            .zip(requests)
            .map(|(reply, request)| {
                assert_eq!(reply["ok"], json!(true), "{request} failed: {reply}");
                reply["result"].clone()
            })
            .collect()
    }

    fn codes(&self, root: &Path, folder: bool, keyed: bool, requests: &[Value]) -> Vec<String> {
        self.session(root, folder, keyed, requests)
            .into_iter()
            .map(|reply| reply["error"]["code"].as_str().unwrap_or("ok").to_owned())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.database());
        let _ = fs::remove_dir_all(&self.parent);
    }
}

/// The stored `ssn` of `id`, as stored.
fn stored_ssn(root: &Path, id: &str) -> Value {
    let stored = NativeRoot::open(root)
        .unwrap()
        .collection(PII)
        .unwrap()
        .read_document(id)
        .unwrap();
    serde_json::from_slice::<Value>(&stored.bytes).unwrap()["ssn"].clone()
}

fn is_ciphertext(value: &Value) -> bool {
    value.as_str().is_some_and(|text| text.starts_with("v2."))
}

fn put(id: &str, name: &str, ssn: &str) -> Value {
    json!({"op":"putData","collection":PII,"id":id,"data":{"name":name,"ssn":ssn}})
}

fn recorded(root: &Path) -> Option<Vec<String>> {
    NativeRoot::open(root)
        .unwrap()
        .recorded_encrypted_fields(PII)
        .unwrap()
}

/// Create `pii` with a keyed session that has the folder, and write one
/// document, which records `ssn`.
fn record_with_the_folder(fixture: &Fixture, root: &Path, create: Value) {
    fixture.succeed(root, true, true, &[create, put(CY, "Cy", "4444")]);
    assert_eq!(recorded(root), Some(vec!["ssn".to_owned()]));
    assert!(is_ciphertext(&stored_ssn(root, CY)));
}

/// Hold every writer without the folder to the record of `root`, where
/// `pii` holds only `CY`.
fn writers_without_the_folder_honour_the_record(fixture: &Fixture, root: &Path) {
    // Without the folder or the key: every write that would store `ssn`
    // in plaintext is refused, and nothing is written.
    let refused = fixture.codes(
        root,
        false,
        false,
        &[
            put(BO, "Bo", "9999"),
            json!({"op":"patchDoc","collection":PII,"id":CY,"newDoc":{"ssn":"8888"}}),
            json!({"op":"executeSQL","sql":"UPDATE pii SET ssn = '7777' WHERE name = 'Cy'"}),
            json!({"op":"executeSQL","sql":"INSERT INTO pii (name, ssn) VALUES ('Di', '6666')"}),
        ],
    );
    assert_eq!(refused, vec!["EENGINE_ENCRYPTION"; 4]);
    let ids = fixture.succeed(
        root,
        false,
        true,
        &[json!({"op":"findDocs","collection":PII,"query":{"$onlyIds":true}})],
    );
    assert_eq!(ids[0], json!([CY]));
    assert!(is_ciphertext(&stored_ssn(root, CY)));

    // With the key but without the folder: the record is enough to encrypt
    // and to decrypt.
    let read = fixture.succeed(
        root,
        false,
        true,
        &[
            put(AL, "Al", "5555"),
            json!({"op":"patchDoc","collection":PII,"id":CY,"newDoc":{"name":"Cy","ssn":"3333"}}),
            json!({"op":"getDoc","collection":PII,"id":AL}),
            json!({"op":"getDoc","collection":PII,"id":CY}),
        ],
    );
    assert_eq!(read[2][AL]["ssn"], "5555");
    assert_eq!(read[3][CY]["ssn"], "3333");
    assert!(is_ciphertext(&stored_ssn(root, AL)));
    assert!(is_ciphertext(&stored_ssn(root, CY)));
}

#[test]
fn a_directory_root_records_folder_declared_fields_for_writers_without_the_folder() {
    let fixture = Fixture::new();
    let root = fixture.directory();
    record_with_the_folder(
        &fixture,
        &root,
        json!({"op":"createCollection","collection":PII}),
    );
    writers_without_the_folder_honour_the_record(&fixture, &root);
}

#[test]
fn a_segment_collection_records_folder_declared_fields_for_writers_without_the_folder() {
    let fixture = Fixture::new();
    let root = fixture.directory();
    record_with_the_folder(
        &fixture,
        &root,
        json!({"op":"createCollection","collection":PII,"layout":"segments"}),
    );
    writers_without_the_folder_honour_the_record(&fixture, &root);
}

/// A directory root whose `pii` records `ssn`, packed into the database.
fn packed_database(fixture: &Fixture) -> PathBuf {
    let root = fixture.directory();
    record_with_the_folder(
        fixture,
        &root,
        json!({"op":"createCollection","collection":PII}),
    );
    let database = fixture.database();
    NativeWriteRoot::open(&root)
        .unwrap()
        .pack_database(&database, true)
        .unwrap();
    database
}

#[test]
fn a_packed_database_holds_writers_without_the_folder_to_the_record() {
    let fixture = Fixture::new();
    let database = packed_database(&fixture);
    assert_eq!(recorded(&database), Some(vec!["ssn".to_owned()]));
    writers_without_the_folder_honour_the_record(&fixture, &database);
}

#[test]
fn unpacking_carries_the_record_back_to_a_directory_root() {
    let fixture = Fixture::new();
    let database = packed_database(&fixture);
    let unpacked = fixture.parent.join("unpacked");
    NativeWriteRoot::open(&database)
        .unwrap()
        .unpack_database(&unpacked)
        .unwrap();
    assert_eq!(recorded(&unpacked), Some(vec!["ssn".to_owned()]));
    writers_without_the_folder_honour_the_record(&fixture, &unpacked);
}

#[test]
fn a_database_rewrites_its_record_in_place() {
    let fixture = Fixture::new();
    let database = packed_database(&fixture);
    NativeWriteRoot::open(&database)
        .unwrap()
        .record_encrypted_fields(PII, &["ssn".to_owned(), "phone".to_owned()])
        .unwrap();
    // Another open of the file sees the new descriptor, and the collection
    // is otherwise as it was.
    assert_eq!(
        recorded(&database),
        Some(vec!["ssn".to_owned(), "phone".to_owned()])
    );
    let read = fixture.succeed(
        &database,
        false,
        true,
        &[
            json!({"op":"getDoc","collection":PII,"id":CY}),
            json!({"op":"putData","collection":PII,"id":BO,"data":{"name":"Bo","ssn":"9999","phone":"555"}}),
        ],
    );
    assert_eq!(read[0][CY]["ssn"], "4444");
    let stored = NativeRoot::open(&database)
        .unwrap()
        .collection(PII)
        .unwrap()
        .read_document(BO)
        .unwrap();
    let stored: Value = serde_json::from_slice(&stored.bytes).unwrap();
    assert!(
        is_ciphertext(&stored["phone"]) && is_ciphertext(&stored["ssn"]),
        "{stored}"
    );
    assert_eq!(stored["name"], "Bo");
}

#[test]
fn creating_the_collection_records_its_fields_before_any_write() {
    let fixture = Fixture::new();
    let root = fixture.directory();
    fixture.succeed(
        &root,
        true,
        true,
        &[json!({"op":"createCollection","collection":PII})],
    );
    assert_eq!(recorded(&root), Some(vec!["ssn".to_owned()]));
    assert_eq!(
        fixture.codes(&root, false, false, &[put(BO, "Bo", "9999")]),
        vec!["EENGINE_ENCRYPTION"]
    );
}

#[test]
fn a_collection_created_without_the_record_is_recorded_on_its_next_keyed_write() {
    let fixture = Fixture::new();
    let root = fixture.directory();
    // The folder's one-time import runs before `pii` exists, so `pii` gets
    // no registry from it; created without the key, as before the record
    // existed, it records nothing until a keyed writer with the folder
    // writes.
    fixture.succeed(
        &root,
        true,
        true,
        &[json!({"op":"createCollection","collection":"other"})],
    );
    fixture.succeed(
        &root,
        false,
        false,
        &[json!({"op":"createCollection","collection":PII})],
    );
    assert_eq!(recorded(&root), None);
    fixture.succeed(&root, true, true, &[put(CY, "Cy", "4444")]);
    assert_eq!(recorded(&root), Some(vec!["ssn".to_owned()]));
    writers_without_the_folder_honour_the_record(&fixture, &root);
}

#[test]
fn a_keyed_writer_with_the_folder_keeps_the_record_current() {
    let fixture = Fixture::new();
    let root = fixture.directory();
    fixture.succeed(
        &root,
        true,
        true,
        &[
            json!({"op":"createCollection","collection":PII}),
            put(CY, "Cy", "4444"),
        ],
    );
    // The folder now declares `name` encrypted instead of `ssn`.
    fixture.declare(&json!({ "$encrypted": ["name"] }));
    fixture.succeed(&root, true, true, &[put(BO, "Bo", "9999")]);
    assert_eq!(recorded(&root), Some(vec!["name".to_owned()]));
    // And no longer declares anything: the record empties, and a writer
    // without the folder may store plaintext again.
    fixture.declare(&json!({ "name": "^.+$" }));
    fixture.succeed(&root, true, true, &[put(AL, "Al", "5555")]);
    assert_eq!(recorded(&root), Some(Vec::new()));
    fixture.succeed(&root, false, false, &[put("4VRNF52JPCR", "Ed", "1111")]);
}

#[test]
fn a_dropped_collection_takes_its_record_with_it() {
    let fixture = Fixture::new();
    let directory = fixture.directory();
    record_with_the_folder(
        &fixture,
        &directory,
        json!({"op":"createCollection","collection":PII}),
    );
    let database = fixture.parent.join("dropped.fxdb");
    NativeWriteRoot::open(&directory)
        .unwrap()
        .pack_database(&database, true)
        .unwrap();
    for root in [directory, database] {
        fixture.succeed(
            &root,
            false,
            false,
            &[
                json!({"op":"dropCollection","collection":PII}),
                json!({"op":"createCollection","collection":PII}),
                put(BO, "Bo", "9999"),
            ],
        );
        assert_eq!(recorded(&root), None, "{}", root.display());
        let _ = NativeRoot::close_database(&root);
    }
}
