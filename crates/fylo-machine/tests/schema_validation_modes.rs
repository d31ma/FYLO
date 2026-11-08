//! A collection's validation mode decides what every body write does about
//! its schema (ADR 0017 section 2): `strict` refuses a document the schema
//! rejects, `warn` stores it and counts the violation, `off` checks nothing.
//! The mode lives in the collection's catalog descriptor; a descriptor that
//! records none follows the host's `strict_schema` setting.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, InvocationOptions, InvocationRuntime};
use fylo_storage_native::{NativeRoot, RootConfig, RootOpenOptions};
use serde_json::{Value, json};

const ID: &str = "4VRNF52JPCO";
const OTHER_ID: &str = "4VRNF52JPCP";
const KEY: &str = "fixture-key-at-least-thirty-two-bytes";
const PEOPLE: &str = "people";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    schema: PathBuf,
}

impl Fixture {
    /// A root with an empty `people` collection whose head schema `v1`
    /// requires a capitalised `name`, an `age` of digits and an encrypted
    /// `ssn` of digits.
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-schema-modes-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let root = parent.join("root");
        let schemas = parent.join("schemas");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(schemas.join(PEOPLE).join("history")).unwrap();
        fs::write(
            schemas.join(PEOPLE).join("manifest.json"),
            br#"{"current":"v1","versions":[{"v":"v1"}]}"#,
        )
        .unwrap();
        fs::write(
            schemas.join(PEOPLE).join("history/v1.schema.json"),
            serde_json::to_vec(&json!({
                "$encrypted": ["ssn"],
                "name": "^[A-Z][a-z]+$",
                "age": "^[0-9]+$",
                "ssn?": "^[0-9]+$"
            }))
            .unwrap(),
        )
        .unwrap();
        let fixture = Self {
            parent,
            root,
            schema: schemas,
        };
        success(
            &fixture.runtime(false),
            &json!({"op":"createCollection","collection":PEOPLE}),
        );
        fixture
    }

    /// A fresh invocation runtime: each one reopens the root.
    fn runtime(&self, strict: bool) -> InvocationRuntime {
        let application = ApplicationConfig {
            root: Some(self.root.clone()),
            storage: RootConfig {
                strict_schema: strict,
                ..Default::default()
            },
            schema: Some(self.schema.clone()),
            credentials: Some((KEY.into(), "fixture-salt".into())),
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

    /// The document body exactly as stored, without decrypting it, or
    /// `None` when there is no such document.
    fn stored(&self, identifier: &str) -> Option<Value> {
        let collection = NativeRoot::open(&self.root)
            .unwrap()
            .collection(PEOPLE)
            .unwrap();
        collection
            .read_document(identifier)
            .ok()
            .map(|stored| serde_json::from_slice(&stored.bytes).unwrap())
    }

    fn stored_ids(&self) -> Vec<String> {
        NativeRoot::open(&self.root)
            .unwrap()
            .collection(PEOPLE)
            .unwrap()
            .document_ids()
            .unwrap()
    }

    fn descriptor(&self) -> Value {
        serde_json::from_slice(
            &fs::read(
                self.root
                    .join(".fylo-catalog/collections")
                    .join(format!("{PEOPLE}.json")),
            )
            .unwrap(),
        )
        .unwrap()
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

fn set_mode(runtime: &InvocationRuntime, mode: &str) -> Value {
    success(
        runtime,
        &json!({"op":"schemaMode","collection":PEOPLE,"mode":mode}),
    )
}

fn violations(runtime: &InvocationRuntime) -> u64 {
    let inspected = success(
        runtime,
        &json!({"op":"inspectCollection","collection":PEOPLE}),
    );
    inspected["schemaViolations"].as_u64().unwrap()
}

fn put(id: &str, data: &Value) -> Value {
    json!({"op":"putData","collection":PEOPLE,"id":id,"data":data})
}

fn patch(id: &str, changes: &Value) -> Value {
    json!({"op":"patchDoc","collection":PEOPLE,"id":id,"newDoc":changes})
}

fn sql(statement: &str) -> Value {
    json!({ "op": "executeSQL", "sql": statement })
}

fn is_ciphertext(value: &Value) -> bool {
    value.as_str().is_some_and(|text| text.starts_with("v2."))
}

#[test]
fn strict_mode_refuses_invalid_puts_patches_and_sql_and_writes_nothing() {
    let fixture = Fixture::new();
    // The host is not strict; the recorded mode is what applies.
    let runtime = fixture.runtime(false);
    set_mode(&runtime, "strict");

    assert_eq!(
        refused(&runtime, &put(ID, &json!({"name":"lower","age":"1"}))),
        "ESCHEMA"
    );
    assert_eq!(fixture.stored(ID), None);
    assert_eq!(
        refused(
            &runtime,
            &json!({"op":"batchPutData","collection":PEOPLE,"batch":[{"name":"Bad!","age":"1"}]}),
        ),
        "ESCHEMA"
    );
    assert_eq!(
        refused(
            &runtime,
            &sql("INSERT INTO people (name, age) VALUES ('lower', 'x')")
        ),
        "ESCHEMA"
    );
    assert!(fixture.stored_ids().is_empty());

    success(&runtime, &put(ID, &json!({"name":"Ada","age":"36"})));
    let before = fixture.stored(ID).unwrap();
    assert_eq!(
        refused(&runtime, &patch(ID, &json!({"age":"old"}))),
        "ESCHEMA"
    );
    assert_eq!(
        refused(
            &runtime,
            &json!({"op":"patchDocs","collection":PEOPLE,"update":{"$where":{"$ops":[{"name":{"$eq":"Ada"}}]},"$set":{"extra":"x"}}}),
        ),
        "ESCHEMA"
    );
    assert_eq!(
        refused(
            &runtime,
            &sql("UPDATE people SET age = 'x' WHERE name = 'Ada'")
        ),
        "ESCHEMA"
    );
    assert_eq!(fixture.stored(ID).unwrap(), before);
    assert_eq!(violations(&runtime), 0);
}

#[test]
fn warn_mode_stores_invalid_documents_and_counts_each_violation() {
    let fixture = Fixture::new();
    // The host is strict; the recorded mode still wins.
    let runtime = fixture.runtime(true);
    set_mode(&runtime, "warn");

    success(
        &runtime,
        &put(ID, &json!({"name":"lower","age":"1","ssn":"123"})),
    );
    let stored = fixture.stored(ID).unwrap();
    assert_eq!(stored["name"], "lower");
    assert_eq!(stored["_v"], "v1", "{stored}");
    assert!(is_ciphertext(&stored["ssn"]), "{stored}");
    assert_eq!(violations(&runtime), 1);

    success(&runtime, &patch(ID, &json!({"age":"old"})));
    assert_eq!(fixture.stored(ID).unwrap()["age"], "old");
    assert_eq!(violations(&runtime), 2);

    let inserted = success(
        &runtime,
        &sql("INSERT INTO people (name, age) VALUES ('Grace', 'x')"),
    );
    let inserted = inserted.as_str().unwrap();
    assert_eq!(
        fixture.stored(inserted).unwrap(),
        json!({"name":"Grace","age":"x","_v":"v1"})
    );
    assert_eq!(violations(&runtime), 3);

    // Both rows fail, so both count, once each.
    let updated = success(&runtime, &sql("UPDATE people SET extra = 'y'"));
    assert_eq!(updated, 2);
    assert_eq!(fixture.stored(inserted).unwrap()["extra"], "y");
    assert_eq!(violations(&runtime), 5);

    let patched = success(
        &runtime,
        &json!({"op":"patchDocs","collection":PEOPLE,"update":{"$where":{"$ops":[{"name":{"$eq":"Grace"}}]},"$set":{"age":"1"}}}),
    );
    assert_eq!(patched, 1);
    // Still invalid: `extra` is not in the schema.
    assert_eq!(violations(&runtime), 6);

    // A valid write is not a violation.
    success(&runtime, &put(OTHER_ID, &json!({"name":"Valid","age":"2"})));
    assert_eq!(violations(&runtime), 6);
    let mode = success(&runtime, &json!({"op":"schemaMode","collection":PEOPLE}));
    assert_eq!(mode["schemaViolations"], 6);
    assert_eq!(mode["validation"], "warn");

    // A write that fails for another reason counts nothing.
    assert_eq!(
        refused(&runtime, &put(ID, &json!({"name":"lower","age":"1"}))),
        "ENATIVE_METADATA"
    );
    assert_eq!(violations(&runtime), 6);
}

#[test]
fn off_mode_checks_nothing_but_still_stamps_and_encrypts() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime(true);
    set_mode(&runtime, "off");

    success(
        &runtime,
        &put(ID, &json!({"name":"lower","age":"x","ssn":"1"})),
    );
    let stored = fixture.stored(ID).unwrap();
    assert_eq!(stored["_v"], "v1", "{stored}");
    assert!(is_ciphertext(&stored["ssn"]), "{stored}");

    success(&runtime, &patch(ID, &json!({"extra":true,"ssn":"2"})));
    let stored = fixture.stored(ID).unwrap();
    assert_eq!(stored["extra"], true);
    assert!(is_ciphertext(&stored["ssn"]), "{stored}");

    let inserted = success(
        &runtime,
        &sql("INSERT INTO people (name, ssn) VALUES ('lower', '3')"),
    );
    let inserted = fixture.stored(inserted.as_str().unwrap()).unwrap();
    assert_eq!(inserted["_v"], "v1");
    assert!(is_ciphertext(&inserted["ssn"]), "{inserted}");

    success(&runtime, &sql("UPDATE people SET age = 'y'"));
    assert_eq!(fixture.stored(ID).unwrap()["age"], "y");
    assert_eq!(violations(&runtime), 0);

    let read = success(
        &runtime,
        &json!({"op":"getDoc","collection":PEOPLE,"id":ID}),
    );
    assert_eq!(read[ID]["ssn"], "2");
}

#[test]
fn the_mode_persists_across_reopening_the_root() {
    let fixture = Fixture::new();
    {
        let runtime = fixture.runtime(false);
        let mode = set_mode(&runtime, "warn");
        assert_eq!(mode["validation"], "warn");
        assert_eq!(mode["recorded"], "warn");
    }
    assert_eq!(fixture.descriptor()["validation"], "warn");

    // A strict host reopening the root runs the collection in warn mode.
    let runtime = fixture.runtime(true);
    let mode = success(&runtime, &json!({"op":"schemaMode","collection":PEOPLE}));
    assert_eq!(mode["validation"], "warn");
    success(&runtime, &put(ID, &json!({"name":"lower","age":"1"})));
    assert_eq!(fixture.stored(ID).unwrap()["name"], "lower");
    let inspected = success(
        &runtime,
        &json!({"op":"inspectCollection","collection":PEOPLE}),
    );
    assert_eq!(inspected["validation"], "warn");
    drop(runtime);

    {
        let runtime = fixture.runtime(true);
        set_mode(&runtime, "strict");
    }
    // And a non-strict host reopening it runs it in strict mode.
    let runtime = fixture.runtime(false);
    assert_eq!(
        refused(&runtime, &put(OTHER_ID, &json!({"name":"lower","age":"1"}))),
        "ESCHEMA"
    );
}

#[test]
fn a_collection_without_a_recorded_mode_follows_strict_schema() {
    let fixture = Fixture::new();
    let descriptor = fixture.descriptor();
    assert!(descriptor.get("validation").is_none(), "{descriptor}");

    let strict = fixture.runtime(true);
    let mode = success(&strict, &json!({"op":"schemaMode","collection":PEOPLE}));
    assert_eq!(mode["validation"], "strict");
    assert_eq!(mode["recorded"], Value::Null);
    assert_eq!(
        refused(&strict, &put(ID, &json!({"name":"lower","age":"1"}))),
        "ESCHEMA"
    );
    drop(strict);

    let lenient = fixture.runtime(false);
    let mode = success(&lenient, &json!({"op":"schemaMode","collection":PEOPLE}));
    assert_eq!(mode["validation"], "off");
    success(&lenient, &put(ID, &json!({"name":"lower","age":"1"})));
    assert_eq!(violations(&lenient), 0);

    // Reading the mode and writing documents leave the descriptor's bytes as
    // they were.
    assert_eq!(fixture.descriptor(), descriptor);
}

#[test]
fn schema_check_lists_the_documents_the_head_schema_rejects() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime(false);
    success(
        &runtime,
        &put(ID, &json!({"name":"Ada","age":"36","ssn":"123"})),
    );
    success(
        &runtime,
        &put(OTHER_ID, &json!({"name":"lower","age":"36"})),
    );
    // The encrypted field is checked as plaintext.
    success(
        &runtime,
        &put(
            "4VRNF52JPCQ",
            &json!({"name":"Cy","age":"1","ssn":"not digits"}),
        ),
    );

    let checked = success(&runtime, &json!({"op":"schemaCheck","collection":PEOPLE}));
    assert_eq!(checked["checked"], 3);
    assert_eq!(checked["failed"], 2);
    assert_eq!(checked["current"], "v1");
    assert_eq!(checked["validation"], "off");
    assert_eq!(checked["truncated"], false);
    let failing: Vec<&str> = checked["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|document| document["id"].as_str().unwrap())
        .collect();
    assert_eq!(failing, [OTHER_ID, "4VRNF52JPCQ"]);
    assert!(
        checked["documents"][0]["error"]
            .as_str()
            .unwrap()
            .contains("'name'"),
        "{checked}"
    );

    let limited = success(
        &runtime,
        &json!({"op":"schemaCheck","collection":PEOPLE,"limit":1}),
    );
    assert_eq!(limited["failed"], 2);
    assert_eq!(limited["documents"].as_array().unwrap().len(), 1);
    assert_eq!(limited["truncated"], true);
}

#[test]
fn schema_mode_refuses_an_unknown_mode() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime(false);
    assert_eq!(
        refused(
            &runtime,
            &json!({"op":"schemaMode","collection":PEOPLE,"mode":"lenient"}),
        ),
        "EBADREQUEST"
    );
    assert!(fixture.descriptor().get("validation").is_none());
}
