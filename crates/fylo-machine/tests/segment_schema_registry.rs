//! A segment collection's schema registry over the machine protocol (ADR 0017
//! section 3, ADR 0015): `schemaPush` commits each version as a schema record
//! through the collection's commit path, and everything above storage —
//! validation, `_v` stamping, encryption lists, push rules, pull — behaves
//! as it does for a collection on the file layout.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, InvocationOptions, InvocationRuntime};
use fylo_storage_native::{NativeRoot, RootConfig, RootOpenOptions};
use serde_json::{Value, json};

const ID: &str = "4VRNF52JPCO";
const OTHER_ID: &str = "4VRNF52JPCP";
const THIRD_ID: &str = "4VRNF52JPCQ";
const KEY: &str = "fixture-key-at-least-thirty-two-bytes";
const PEOPLE: &str = "people";
static NEXT: AtomicU64 = AtomicU64::new(0);

fn v1() -> Value {
    json!({ "$encrypted": ["ssn"], "fullName": "^.+$", "ssn?": "^[0-9]+$" })
}

fn v2() -> Value {
    json!({ "$encrypted": ["ssn"], "name": "^[A-Z].*$", "ssn?": "^[0-9]+$" })
}

fn rename() -> Value {
    json!({ "steps": [{ "rename": "fullName", "to": "name" }] })
}

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    schema: PathBuf,
}

impl Fixture {
    /// A root with a segment collection `people` and a schema folder whose
    /// `people` collection has `v1` only.
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-segment-schema-registry-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let fixture = Self {
            root: parent.join("root"),
            schema: parent.join("schemas"),
            parent,
        };
        fs::create_dir_all(&fixture.root).unwrap();
        fixture.write_version(&["v1"], "v1", &v1(), None);
        success(
            &fixture.runtime(),
            &json!({"op":"createCollection","collection":PEOPLE,"layout":"segments"}),
        );
        assert_eq!(fixture.head(), None);
        fixture
    }

    fn folder(&self) -> PathBuf {
        self.schema.join(PEOPLE)
    }

    fn write_version(
        &self,
        versions: &[&str],
        label: &str,
        schema: &Value,
        migration: Option<(&str, &Value)>,
    ) {
        let folder = self.folder();
        fs::create_dir_all(folder.join("history")).unwrap();
        let entries: Vec<Value> = versions.iter().map(|v| json!({ "v": v })).collect();
        fs::write(
            folder.join("manifest.json"),
            serde_json::to_vec(&json!({ "current": label, "versions": entries })).unwrap(),
        )
        .unwrap();
        fs::write(
            folder.join("history").join(format!("{label}.schema.json")),
            serde_json::to_vec(schema).unwrap(),
        )
        .unwrap();
        if let Some((from, steps)) = migration {
            fs::create_dir_all(folder.join("migrations")).unwrap();
            fs::write(
                folder
                    .join("migrations")
                    .join(format!("{from}-to-{label}.json")),
                serde_json::to_vec(steps).unwrap(),
            )
            .unwrap();
        }
    }

    fn add_v2(&self) {
        self.write_version(&["v1", "v2"], "v2", &v2(), Some(("v1", &rename())));
    }

    fn runtime(&self) -> InvocationRuntime {
        let application = ApplicationConfig {
            root: Some(self.root.clone()),
            storage: RootConfig {
                strict_schema: true,
                ..Default::default()
            },
            schema: Some(self.schema.clone()),
            credentials: Some((KEY.into(), "fixture-salt".into())),
            hot_set_bytes: None,
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

    fn head(&self) -> Option<u32> {
        NativeRoot::open(&self.root)
            .unwrap()
            .schema_registry_head(PEOPLE)
            .unwrap()
    }

    fn version_record(&self, ordinal: u32) -> Value {
        serde_json::from_slice(
            &NativeRoot::open(&self.root)
                .unwrap()
                .read_schema_version(PEOPLE, ordinal)
                .unwrap(),
        )
        .unwrap()
    }

    /// The document body exactly as stored, without decrypting it.
    fn stored(&self, identifier: &str) -> Value {
        let stored = NativeRoot::open(&self.root)
            .unwrap()
            .collection(PEOPLE)
            .unwrap()
            .read_document(identifier)
            .unwrap();
        serde_json::from_slice(&stored.bytes).unwrap()
    }

    fn segments(&self) -> PathBuf {
        self.root.join(".collections").join(PEOPLE).join("segments")
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

fn refused(runtime: &InvocationRuntime, request: &Value) -> (String, String) {
    let response = execute(runtime, request);
    assert_eq!(response["ok"], false, "{response}");
    (
        response["error"]["code"].as_str().unwrap().to_owned(),
        response["error"]["message"].as_str().unwrap().to_owned(),
    )
}

fn push() -> Value {
    json!({"op":"schemaPush","collection":PEOPLE})
}

fn put(id: &str, data: &Value) -> Value {
    json!({"op":"putData","collection":PEOPLE,"id":id,"data":data})
}

fn labels(report: &Value) -> Vec<String> {
    report["pushed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|version| version["label"].as_str().unwrap().to_owned())
        .collect()
}

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn push_and_pull_round_trip_a_segment_collection() {
    let fixture = Fixture::new();
    fixture.add_v2();
    let runtime = fixture.runtime();
    let report = success(&runtime, &push());
    assert_eq!(labels(&report), ["v1", "v2"], "{report}");
    assert_eq!(report["head"], "v2", "{report}");
    assert_eq!(fixture.head(), Some(2));
    // The versions are records in the collection, not registry files.
    assert!(!fixture.root.join(".fylo-schemas").join(PEOPLE).exists());
    assert!(fixture.segments().join("schemas.snapshot").exists());
    let first = fixture.version_record(1);
    assert_eq!(first["format"], "fylo.schema-version.v1", "{first}");
    assert_eq!(first["label"], "v1", "{first}");
    assert_eq!(first["chex"], v1(), "{first}");
    assert_eq!(first["portable"], true, "{first}");
    assert!(
        first["compiled"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "{first}"
    );
    assert_eq!(fixture.version_record(2)["migration"], rename());

    let again = success(&runtime, &push());
    assert!(labels(&again).is_empty(), "{again}");
    assert_eq!(again["unchanged"], 2, "{again}");

    let out = fixture.parent.join("pulled");
    let pulled = success(
        &runtime,
        &json!({"op":"schemaPull","collection":PEOPLE,"out":out}),
    );
    assert_eq!(pulled["current"], "v2", "{pulled}");
    assert_eq!(pulled["versions"], 2, "{pulled}");
    let folder = out.join(PEOPLE);
    assert_eq!(read_json(&folder.join("history/v1.schema.json")), v1());
    assert_eq!(read_json(&folder.join("history/v2.schema.json")), v2());
    assert_eq!(
        read_json(&folder.join("migrations/v1-to-v2.json")),
        rename()
    );

    let inspect = success(&runtime, &json!({"op":"schemaInspect","collection":PEOPLE}));
    assert_eq!(inspect["source"], "registry", "{inspect}");
    assert_eq!(inspect["current"], "v2", "{inspect}");
}

#[test]
fn a_committed_label_with_new_content_is_a_conflict() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime();
    success(&runtime, &push());
    fixture.write_version(
        &["v1"],
        "v1",
        &json!({ "$encrypted": ["ssn"], "fullName": "^[A-Z].*$" }),
        None,
    );
    let (code, message) = refused(&runtime, &push());
    assert_eq!(code, "ESCHEMACONFLICT", "{message}");
    assert_eq!(fixture.head(), Some(1));
}

#[test]
fn a_document_commit_after_a_schema_commit_validates_against_the_new_head() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime();
    success(&runtime, &push());
    success(
        &runtime,
        &put(ID, &json!({"fullName":"ada lovelace","ssn":"42"})),
    );
    let stored = fixture.stored(ID);
    assert_eq!(stored["_v"], "v1", "{stored}");
    assert_ne!(stored["ssn"], "42", "ssn is stored encrypted: {stored}");

    // The dry run refuses v2 while a document would fail it.
    fixture.add_v2();
    let (code, message) = refused(&runtime, &push());
    assert_eq!(code, "ESCHEMA", "{message}");
    assert_eq!(fixture.head(), Some(1));
    success(
        &runtime,
        &json!({"op":"patchDoc","collection":PEOPLE,"id":ID,"newDoc":{"fullName":"Ada Lovelace"}}),
    );
    let report = success(&runtime, &push());
    assert_eq!(labels(&report), ["v2"], "{report}");
    assert_eq!(fixture.head(), Some(2));

    // The same runtime, and a fresh one, validate the next write at v2.
    for runtime in [&runtime, &fixture.runtime()] {
        let (code, message) = refused(runtime, &put(OTHER_ID, &json!({"fullName":"Grace"})));
        assert_eq!(code, "ESCHEMA", "{message}");
    }
    success(&runtime, &put(OTHER_ID, &json!({"name":"Grace","ssn":"7"})));
    let stored = fixture.stored(OTHER_ID);
    assert_eq!(stored["_v"], "v2", "{stored}");
    assert_ne!(stored["ssn"], "7", "{stored}");
    let document = success(
        &runtime,
        &json!({"op":"getDoc","collection":PEOPLE,"id":OTHER_ID,"onlyId":false}),
    );
    assert_eq!(document[OTHER_ID]["ssn"], "7", "{document}");
    // The v1 document still decrypts at its own version.
    let document = success(
        &runtime,
        &json!({"op":"getDoc","collection":PEOPLE,"id":ID,"onlyId":false}),
    );
    assert_eq!(document[ID]["ssn"], "42", "{document}");
}

#[test]
fn compaction_and_recovery_keep_the_registry() {
    let fixture = Fixture::new();
    fixture.add_v2();
    let runtime = fixture.runtime();
    success(&runtime, &push());
    for round in 0..4 {
        for id in [ID, OTHER_ID, THIRD_ID] {
            let name = format!("Name{round}");
            if round == 0 {
                success(&runtime, &put(id, &json!({ "name": name })));
            } else {
                success(
                    &runtime,
                    &json!({"op":"patchDoc","collection":PEOPLE,"id":id,"newDoc":{"name":name}}),
                );
            }
        }
    }
    success(
        &runtime,
        &json!({"op":"compactCollection","collection":PEOPLE}),
    );
    drop(runtime);
    let runtime = fixture.runtime();
    assert_eq!(fixture.head(), Some(2));
    let again = success(&runtime, &push());
    assert!(labels(&again).is_empty(), "{again}");
    let (code, _) = refused(&runtime, &put("4VRNF52JPCR", &json!({"name":"lower"})));
    assert_eq!(code, "ESCHEMA");
    success(&runtime, &put("4VRNF52JPCR", &json!({"name":"Upper"})));
    assert_eq!(fixture.stored("4VRNF52JPCR")["_v"], "v2");
}
