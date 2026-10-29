//! Every machine mutation that writes a document body validates it against the
//! head schema and encrypts its declared fields (ADR 0017 section 1).
//!
//! `putData` always went through the write engine. `patchDoc`, `patchDocs` and
//! SQL `INSERT` and `UPDATE` used to reach the storage writer directly, which
//! stored invalid documents and wrote `$encrypted` fields in plaintext.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, InvocationOptions, InvocationRuntime};
use fylo_storage_native::{NativeRoot, RootConfig, RootOpenOptions};
use serde_json::{Value, json};

const ID: &str = "4VRNF52JPCO";
const KEY: &str = "fixture-key-at-least-thirty-two-bytes";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    schema: PathBuf,
}

impl Fixture {
    /// A root and a schema folder whose `collection` head version is `schema`.
    fn new(collection: &str, schema: &Value) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-schema-writes-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let root = parent.join("root");
        let schemas = parent.join("schemas");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(schemas.join(collection).join("history")).unwrap();
        fs::write(
            schemas.join(collection).join("manifest.json"),
            br#"{"current":"v1","versions":[{"v":"v1"}]}"#,
        )
        .unwrap();
        fs::write(
            schemas.join(collection).join("history/v1.schema.json"),
            serde_json::to_vec(schema).unwrap(),
        )
        .unwrap();
        Self {
            parent,
            root,
            schema: schemas,
        }
    }

    fn runtime(&self, strict: bool, credentials: bool) -> InvocationRuntime {
        let application = ApplicationConfig {
            root: Some(self.root.clone()),
            storage: RootConfig {
                strict_schema: strict,
                ..Default::default()
            },
            schema: Some(self.schema.clone()),
            credentials: credentials.then(|| (KEY.into(), "fixture-salt".into())),
            hot_set_bytes: None,
            ..ApplicationConfig::default()
        };
        let options = InvocationOptions::new(
            RootOpenOptions::new(&self.root),
            application,
            FrameLimits::default(),
            (
                Instant::now() + Duration::from_secs(30),
                Duration::from_millis(100),
            ),
        )
        .unwrap();
        InvocationRuntime::new(options).unwrap()
    }

    /// The document body exactly as stored, without decrypting it.
    fn stored(&self, collection: &str, identifier: &str) -> Value {
        let stored = NativeRoot::open(&self.root)
            .unwrap()
            .collection(collection)
            .unwrap()
            .read_document(identifier)
            .unwrap();
        serde_json::from_slice(&stored.bytes).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn execute(runtime: &InvocationRuntime, request: &Value) -> Value {
    let mut frame = serde_json::to_vec(request).unwrap();
    frame.push(b'\n');
    serde_json::from_slice(&runtime.execute(&frame).unwrap()).unwrap()
}

fn success(runtime: &InvocationRuntime, request: &Value) -> Value {
    let response = execute(runtime, request);
    assert_eq!(response["ok"], true, "{response}");
    response["result"].clone()
}

fn refused(runtime: &InvocationRuntime, request: &Value) -> String {
    let response = execute(runtime, request);
    assert_eq!(response["ok"], false, "{response}");
    response["error"]["code"].as_str().unwrap().to_owned()
}

fn sql(statement: &str) -> Value {
    json!({ "op": "executeSQL", "sql": statement })
}

fn is_ciphertext(value: &Value) -> bool {
    value.as_str().is_some_and(|text| text.starts_with("v2."))
}

const PEOPLE: &str = "people";

fn people() -> Fixture {
    Fixture::new(
        PEOPLE,
        &json!({ "name": "^[A-Z][a-z]+$", "age": "^[0-9]+$" }),
    )
}

#[test]
fn strict_patch_doc_refuses_an_invalid_merge_and_leaves_the_document() {
    let fixture = people();
    let runtime = fixture.runtime(true, false);
    success(
        &runtime,
        &json!({"op":"createCollection","collection":PEOPLE}),
    );
    success(
        &runtime,
        &json!({"op":"putData","collection":PEOPLE,"id":ID,"data":{"name":"Ada","age":"36"}}),
    );
    let before = fixture.stored(PEOPLE, ID);
    let code = refused(
        &runtime,
        &json!({"op":"patchDoc","collection":PEOPLE,"id":ID,"newDoc":{"name":"!!bad"}}),
    );
    assert_eq!(code, "ESCHEMA");
    let code = refused(
        &runtime,
        &json!({"op":"patchDoc","collection":PEOPLE,"id":ID,"newDoc":{"extra":"x"}}),
    );
    assert_eq!(code, "ESCHEMA");
    assert_eq!(fixture.stored(PEOPLE, ID), before);
}

#[test]
fn strict_patch_doc_validates_the_whole_merged_document_and_stamps_the_head() {
    let fixture = people();
    let runtime = fixture.runtime(true, false);
    success(
        &runtime,
        &json!({"op":"createCollection","collection":PEOPLE}),
    );
    success(
        &runtime,
        &json!({"op":"putData","collection":PEOPLE,"id":ID,"data":{"name":"Ada","age":"36"}}),
    );
    success(
        &runtime,
        &json!({"op":"patchDoc","collection":PEOPLE,"id":ID,"newDoc":{"age":"37"}}),
    );
    assert_eq!(
        fixture.stored(PEOPLE, ID),
        json!({"name":"Ada","age":"37","_v":"v1"})
    );
}

#[test]
fn strict_patch_docs_refuses_an_invalid_merge() {
    let fixture = people();
    let runtime = fixture.runtime(true, false);
    success(
        &runtime,
        &json!({"op":"createCollection","collection":PEOPLE}),
    );
    success(
        &runtime,
        &json!({"op":"putData","collection":PEOPLE,"id":ID,"data":{"name":"Ada","age":"36"}}),
    );
    let before = fixture.stored(PEOPLE, ID);
    let code = refused(
        &runtime,
        &json!({"op":"patchDocs","collection":PEOPLE,"update":{"$where":{"name":{"$eq":"Ada"}},"$set":{"age":"NaN"}}}),
    );
    assert_eq!(code, "ESCHEMA");
    assert_eq!(fixture.stored(PEOPLE, ID), before);
}

#[test]
fn strict_sql_insert_validates_and_stamps_the_head() {
    let fixture = people();
    let runtime = fixture.runtime(true, false);
    success(
        &runtime,
        &json!({"op":"createCollection","collection":PEOPLE}),
    );
    let code = refused(
        &runtime,
        &sql("INSERT INTO people (name, age) VALUES ('lower', 'abc')"),
    );
    assert_eq!(code, "ESCHEMA");
    let identifier = success(
        &runtime,
        &sql("INSERT INTO people (name, age) VALUES ('Grace', '85')"),
    );
    assert_eq!(
        fixture.stored(PEOPLE, identifier.as_str().unwrap()),
        json!({"name":"Grace","age":"85","_v":"v1"})
    );
}

#[test]
fn strict_sql_update_refuses_an_invalid_assignment() {
    let fixture = people();
    let runtime = fixture.runtime(true, false);
    success(
        &runtime,
        &json!({"op":"createCollection","collection":PEOPLE}),
    );
    success(
        &runtime,
        &json!({"op":"putData","collection":PEOPLE,"id":ID,"data":{"name":"Ada","age":"36"}}),
    );
    let before = fixture.stored(PEOPLE, ID);
    let code = refused(
        &runtime,
        &sql("UPDATE people SET age = 'zzz' WHERE name = 'Ada'"),
    );
    assert_eq!(code, "ESCHEMA");
    assert_eq!(fixture.stored(PEOPLE, ID), before);
    success(
        &runtime,
        &sql("UPDATE people SET age = '40' WHERE name = 'Ada'"),
    );
    assert_eq!(fixture.stored(PEOPLE, ID)["age"], "40");
}

#[test]
fn non_strict_writes_still_accept_documents_outside_the_schema() {
    let fixture = people();
    let runtime = fixture.runtime(false, false);
    success(
        &runtime,
        &json!({"op":"createCollection","collection":PEOPLE}),
    );
    success(
        &runtime,
        &json!({"op":"putData","collection":PEOPLE,"id":ID,"data":{"name":"Ada","age":"36"}}),
    );
    success(
        &runtime,
        &json!({"op":"patchDoc","collection":PEOPLE,"id":ID,"newDoc":{"name":"!!bad"}}),
    );
    success(
        &runtime,
        &sql("INSERT INTO people (name, age) VALUES ('lower', 'abc')"),
    );
    assert_eq!(fixture.stored(PEOPLE, ID)["name"], "!!bad");
}

const PII: &str = "pii";

fn pii() -> Fixture {
    Fixture::new(
        PII,
        &json!({ "$encrypted": ["ssn"], "name": "^.+$", "ssn": "^[0-9-]+$" }),
    )
}

#[test]
fn patch_doc_encrypts_a_declared_field() {
    let fixture = pii();
    let runtime = fixture.runtime(false, true);
    success(&runtime, &json!({"op":"createCollection","collection":PII}));
    success(
        &runtime,
        &json!({"op":"putData","collection":PII,"id":ID,"data":{"name":"Ada","ssn":"111-22"}}),
    );
    success(
        &runtime,
        &json!({"op":"patchDoc","collection":PII,"id":ID,"newDoc":{"ssn":"333-44"}}),
    );
    let stored = fixture.stored(PII, ID);
    assert!(is_ciphertext(&stored["ssn"]), "{stored}");
    assert_eq!(stored["name"], "Ada");
    let read = success(&runtime, &json!({"op":"getDoc","collection":PII,"id":ID}));
    assert_eq!(read[ID]["ssn"], "333-44");
}

#[test]
fn patch_doc_keeps_other_encrypted_fields_encrypted() {
    let fixture = pii();
    let runtime = fixture.runtime(false, true);
    success(&runtime, &json!({"op":"createCollection","collection":PII}));
    success(
        &runtime,
        &json!({"op":"putData","collection":PII,"id":ID,"data":{"name":"Ada","ssn":"111-22"}}),
    );
    success(
        &runtime,
        &json!({"op":"patchDoc","collection":PII,"id":ID,"newDoc":{"name":"Grace"}}),
    );
    let stored = fixture.stored(PII, ID);
    assert!(is_ciphertext(&stored["ssn"]), "{stored}");
    let read = success(&runtime, &json!({"op":"getDoc","collection":PII,"id":ID}));
    assert_eq!(read[ID], json!({"name":"Grace","ssn":"111-22","_v":"v1"}));
}

#[test]
fn patch_docs_encrypts_a_declared_field() {
    let fixture = pii();
    let runtime = fixture.runtime(false, true);
    success(&runtime, &json!({"op":"createCollection","collection":PII}));
    success(
        &runtime,
        &json!({"op":"putData","collection":PII,"id":ID,"data":{"name":"Ada","ssn":"111-22"}}),
    );
    success(
        &runtime,
        &json!({"op":"patchDocs","collection":PII,"update":{"$where":{"name":{"$eq":"Ada"}},"$set":{"ssn":"555-66"}}}),
    );
    assert!(is_ciphertext(&fixture.stored(PII, ID)["ssn"]));
    let read = success(&runtime, &json!({"op":"getDoc","collection":PII,"id":ID}));
    assert_eq!(read[ID]["ssn"], "555-66");
}

#[test]
fn sql_insert_and_update_encrypt_a_declared_field() {
    let fixture = pii();
    let runtime = fixture.runtime(false, true);
    success(&runtime, &json!({"op":"createCollection","collection":PII}));
    let identifier = success(
        &runtime,
        &sql("INSERT INTO pii (name, ssn) VALUES ('Cy', '4444')"),
    );
    let identifier = identifier.as_str().unwrap();
    assert!(is_ciphertext(&fixture.stored(PII, identifier)["ssn"]));
    success(
        &runtime,
        &sql("UPDATE pii SET ssn = '7777' WHERE name = 'Cy'"),
    );
    assert!(is_ciphertext(&fixture.stored(PII, identifier)["ssn"]));
    let read = success(
        &runtime,
        &json!({"op":"getDoc","collection":PII,"id":identifier}),
    );
    assert_eq!(read[identifier]["ssn"], "7777");
    let found = success(
        &runtime,
        &json!({"op":"findDocs","collection":PII,"query":{}}),
    );
    assert_eq!(found[identifier]["ssn"], "7777");
}

#[test]
fn strict_mode_and_encryption_work_together() {
    let fixture = pii();
    let runtime = fixture.runtime(true, true);
    success(&runtime, &json!({"op":"createCollection","collection":PII}));
    success(
        &runtime,
        &json!({"op":"putData","collection":PII,"id":ID,"data":{"name":"Ada","ssn":"111-22"}}),
    );
    let stored = fixture.stored(PII, ID);
    assert!(is_ciphertext(&stored["ssn"]), "{stored}");
    assert_eq!(stored["_v"], "v1");
    let code = refused(
        &runtime,
        &json!({"op":"patchDoc","collection":PII,"id":ID,"newDoc":{"ssn":"not digits"}}),
    );
    assert_eq!(code, "ESCHEMA");
    let code = refused(
        &runtime,
        &json!({"op":"putData","collection":PII,"id":"4VRNF52JPCP","data":{"name":"Ada","$encrypted":["name"]}}),
    );
    assert_eq!(code, "ESCHEMA");
    success(
        &runtime,
        &json!({"op":"patchDoc","collection":PII,"id":ID,"newDoc":{"ssn":"999"}}),
    );
    let read = success(&runtime, &json!({"op":"getDoc","collection":PII,"id":ID}));
    assert_eq!(read[ID]["ssn"], "999");
}

#[test]
fn encrypted_strings_that_read_as_json_stay_strings() {
    let fixture = Fixture::new(PII, &json!({ "$encrypted": ["a", "b", "c", "d", "e"] }));
    let runtime = fixture.runtime(false, true);
    success(&runtime, &json!({"op":"createCollection","collection":PII}));
    let data = json!({"a":"999","b":"true","c":"null","d":"[1]","e":42});
    success(
        &runtime,
        &json!({"op":"putData","collection":PII,"id":ID,"data":data}),
    );
    let read = success(&runtime, &json!({"op":"getDoc","collection":PII,"id":ID}));
    assert_eq!(
        read[ID],
        json!({"a":"999","b":"true","c":"null","d":"[1]","e":42,"_v":"v1"})
    );
}

/// A segment collection, where SQL `UPDATE` once merged into the stored
/// form directly: neither decrypting, validating nor encrypting.
fn create_segments(runtime: &InvocationRuntime, collection: &str) {
    success(
        runtime,
        &json!({"op":"createCollection","collection":collection,"layout":"segments"}),
    );
}

#[test]
fn sql_update_encrypts_a_declared_field_in_a_segment_collection() {
    let fixture = pii();
    let runtime = fixture.runtime(false, true);
    create_segments(&runtime, PII);
    success(
        &runtime,
        &json!({"op":"putData","collection":PII,"id":ID,"data":{"name":"Cy","ssn":"4444"}}),
    );
    success(
        &runtime,
        &sql("UPDATE pii SET ssn = '7777' WHERE ssn = '4444'"),
    );
    assert!(is_ciphertext(&fixture.stored(PII, ID)["ssn"]));
    let read = success(&runtime, &json!({"op":"getDoc","collection":PII,"id":ID}));
    assert_eq!(read[ID]["ssn"], "7777");
}

#[test]
fn sql_update_without_keys_is_refused_in_an_encrypted_segment_collection() {
    let fixture = pii();
    let keyed = fixture.runtime(false, true);
    create_segments(&keyed, PII);
    success(
        &keyed,
        &json!({"op":"putData","collection":PII,"id":ID,"data":{"name":"Cy","ssn":"4444"}}),
    );
    // Pushed, so a process without the folder knows the field is encrypted.
    success(&keyed, &json!({"op":"schemaPush","collection":PII}));
    drop(keyed);
    let before = fixture.stored(PII, ID);
    let keyless = InvocationRuntime::new(
        InvocationOptions::new(
            RootOpenOptions::new(&fixture.root),
            ApplicationConfig {
                root: Some(fixture.root.clone()),
                hot_set_bytes: None,
                ..ApplicationConfig::default()
            },
            FrameLimits::default(),
            (
                Instant::now() + Duration::from_secs(30),
                Duration::from_millis(100),
            ),
        )
        .unwrap(),
    )
    .unwrap();
    let code = refused(
        &keyless,
        &sql("UPDATE pii SET ssn = '9111' WHERE name = 'Cy'"),
    );
    assert_eq!(code, "EENGINE_ENCRYPTION");
    assert_eq!(fixture.stored(PII, ID), before);
}

#[test]
fn strict_sql_update_refuses_an_invalid_assignment_in_a_segment_collection() {
    let fixture = people();
    let runtime = fixture.runtime(true, false);
    create_segments(&runtime, PEOPLE);
    success(
        &runtime,
        &json!({"op":"putData","collection":PEOPLE,"id":ID,"data":{"name":"Ada","age":"36"}}),
    );
    let before = fixture.stored(PEOPLE, ID);
    let code = refused(
        &runtime,
        &sql("UPDATE people SET age = 'zzz' WHERE name = 'Ada'"),
    );
    assert_eq!(code, "ESCHEMA");
    assert_eq!(fixture.stored(PEOPLE, ID), before);
}
