//! A collection's schema registry (ADR 0017 section 3): `schemaPush` moves a
//! `fylo.schema` folder's versions into the root, `schemaPull` moves them back
//! out, and once a collection has a registry its validation, `_v` stamping,
//! encryption lists and migrations come from there, not the folder.

use std::fs;
use std::path::{Path, PathBuf};
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

/// `v1`: a `fullName` and an encrypted `ssn`. `v2` renames `fullName` to
/// `name` and keeps `ssn` encrypted.
fn v1() -> Value {
    json!({ "$encrypted": ["ssn"], "fullName": "^.+$", "ssn?": "^[0-9]+$" })
}

fn v2() -> Value {
    json!({ "$encrypted": ["ssn"], "name": "^[A-Z].*$", "ssn?": "^[0-9]+$" })
}

fn rename() -> Value {
    json!({ "steps": [{ "rename": "fullName", "to": "name" }] })
}

struct Options {
    schema: bool,
    strict: bool,
    auto_push: bool,
    /// A folder other than the fixture's own.
    folder: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            schema: true,
            strict: true,
            auto_push: false,
            folder: None,
        }
    }
}

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    schema: PathBuf,
}

impl Fixture {
    /// A root and a schema folder whose `people` collection has `v1` only.
    /// A session with the folder creates the collection, so the one-time
    /// import has already run, before the collection existed, and imported
    /// nothing: each test pushes for itself.
    fn new() -> Self {
        Self::create(true)
    }

    /// As [`Self::new`], but the collection is created by a session without
    /// a folder, so the one-time import is still to come.
    fn unimported() -> Self {
        Self::create(false)
    }

    fn create(with_folder: bool) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-schema-registry-{}-{stamp}-{}",
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
            &fixture.runtime_with(&Options {
                schema: with_folder,
                ..Options::default()
            }),
            &json!({"op":"createCollection","collection":PEOPLE}),
        );
        assert_eq!(fixture.head(), None);
        fixture
    }

    fn folder(&self) -> PathBuf {
        self.schema.join(PEOPLE)
    }

    /// Write `schema` as `label`, with `migration` from the version before
    /// it, and a manifest listing `versions` with `label` current.
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
        self.runtime_with(&Options::default())
    }

    /// A fresh invocation runtime: each one reopens the root.
    fn runtime_with(&self, options: &Options) -> InvocationRuntime {
        let application = ApplicationConfig {
            root: Some(self.root.clone()),
            storage: RootConfig {
                strict_schema: options.strict,
                schema_auto_push: options.auto_push,
                ..Default::default()
            },
            schema: options.schema.then(|| {
                options
                    .folder
                    .clone()
                    .unwrap_or_else(|| self.schema.clone())
            }),
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

    fn registry_file(&self, ordinal: u32) -> PathBuf {
        self.root
            .join(".fylo-schemas")
            .join(PEOPLE)
            .join(format!("{ordinal}.json"))
    }

    fn version_record(&self, ordinal: u32) -> Value {
        serde_json::from_slice(&fs::read(self.registry_file(ordinal)).unwrap()).unwrap()
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

/// The error code and message of a refused request.
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

fn get(runtime: &InvocationRuntime, id: &str) -> Value {
    success(
        runtime,
        &json!({"op":"getDoc","collection":PEOPLE,"id":id,"onlyId":false}),
    )
}

fn labels(report: &Value) -> Vec<String> {
    report["pushed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|version| version["label"].as_str().unwrap().to_owned())
        .collect()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn push_commits_each_folder_version_once_and_pull_writes_them_back() {
    let fixture = Fixture::new();
    fixture.add_v2();
    let runtime = fixture.runtime();
    let report = success(&runtime, &push());
    assert_eq!(labels(&report), ["v1", "v2"], "{report}");
    assert_eq!(report["head"], "v2", "{report}");
    assert_eq!(report["unchanged"], 0, "{report}");
    assert_eq!(fixture.head(), Some(2));

    let first = fixture.version_record(1);
    assert_eq!(first["format"], "fylo.schema-version.v1", "{first}");
    assert_eq!(first["ordinal"], 1);
    assert_eq!(first["label"], "v1");
    assert_eq!(first["chex"], v1());
    assert!(first.get("migration").is_none(), "{first}");
    assert_eq!(first["portable"], true, "{first}");
    assert!(
        first["compiled"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "{first}"
    );
    assert_eq!(first["committed"]["actor"], "fylo schema push", "{first}");
    let second = fixture.version_record(2);
    assert_eq!(second["migration"], rename(), "{second}");

    // Nothing is pending, so a second push commits nothing.
    let again = success(&runtime, &push());
    assert!(labels(&again).is_empty(), "{again}");
    assert_eq!(again["unchanged"], 2, "{again}");
    assert_eq!(fixture.head(), Some(2));

    let out = fixture.parent.join("pulled");
    let pulled = success(
        &runtime,
        &json!({"op":"schemaPull","collection":PEOPLE,"out":out}),
    );
    assert_eq!(pulled["current"], "v2", "{pulled}");
    assert_eq!(pulled["versions"], 2, "{pulled}");
    let folder = out.join(PEOPLE);
    let manifest = read_json(&folder.join("manifest.json"));
    assert_eq!(manifest["current"], "v2", "{manifest}");
    assert_eq!(manifest["versions"][0]["v"], "v1", "{manifest}");
    assert_eq!(manifest["versions"][1]["v"], "v2", "{manifest}");
    assert_eq!(read_json(&folder.join("history/v1.schema.json")), v1());
    assert_eq!(read_json(&folder.join("history/v2.schema.json")), v2());
    assert_eq!(
        read_json(&folder.join("migrations/v1-to-v2.json")),
        rename()
    );

    // The pulled folder, reformatted as it is, pushes as the same versions.
    let pulled_runtime = fixture.runtime_with(&Options {
        folder: Some(out.clone()),
        ..Options::default()
    });
    let round_trip = success(&pulled_runtime, &push());
    assert!(labels(&round_trip).is_empty(), "{round_trip}");
    assert_eq!(round_trip["unchanged"], 2, "{round_trip}");
    // The folder's own `doctor` accepts what pull wrote.
    let doctor = success(
        &pulled_runtime,
        &json!({"op":"schemaDoctor","collection":PEOPLE}),
    );
    assert_eq!(doctor["ok"], true, "{doctor}");
    assert!(
        doctor["warnings"].as_array().unwrap().is_empty(),
        "{doctor}"
    );
}

#[test]
fn a_committed_label_with_new_content_is_a_conflict() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime();
    success(&runtime, &push());
    // Reordering keys is the same schema; changing a pattern is not.
    fixture.write_version(
        &["v1"],
        "v1",
        &json!({ "ssn?": "^[0-9]+$", "fullName": "^.+$", "$encrypted": ["ssn"] }),
        None,
    );
    assert!(labels(&success(&runtime, &push())).is_empty());
    fixture.write_version(
        &["v1"],
        "v1",
        &json!({ "$encrypted": ["ssn"], "fullName": "^[A-Z].*$" }),
        None,
    );
    let (code, message) = refused(&runtime, &push());
    assert_eq!(code, "ESCHEMACONFLICT", "{message}");
    assert!(message.contains("immutable"), "{message}");
    assert_eq!(fixture.head(), Some(1));
}

#[test]
fn push_refuses_javascript_upgraders_directives_and_arrays_of_objects() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime();
    success(&runtime, &push());

    fixture.add_v2();
    let upgraders = fixture.folder().join("upgraders");
    fs::create_dir_all(&upgraders).unwrap();
    fs::write(
        upgraders.join("v1-to-v2.js"),
        "export default (doc) => doc\n",
    )
    .unwrap();
    let (code, message) = refused(&runtime, &push());
    assert_eq!(code, "ESCHEMA", "{message}");
    assert!(message.contains("ADR 0018"), "{message}");
    fs::remove_dir_all(&upgraders).unwrap();

    fixture.write_version(
        &["v1", "v2"],
        "v2",
        &json!({ "$unique": ["name"], "name": "^.+$" }),
        Some(("v1", &rename())),
    );
    let (code, message) = refused(&runtime, &push());
    assert_eq!(code, "ESCHEMA", "{message}");
    assert!(message.contains("$unique"), "{message}");

    fixture.write_version(
        &["v1", "v2"],
        "v2",
        &json!({ "name": "^.+$", "tags": [{ "label": "^.+$" }] }),
        Some(("v1", &rename())),
    );
    let (code, message) = refused(&runtime, &push());
    assert_eq!(code, "EARRAYOFOBJECTS", "{message}");
    assert_eq!(fixture.head(), Some(1));
}

#[test]
fn push_refuses_a_version_the_migrated_documents_fail() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime();
    success(&runtime, &push());
    success(&runtime, &put(ID, &json!({"fullName":"Ada"})));
    success(&runtime, &put(OTHER_ID, &json!({"fullName":"grace"})));

    // `v2` wants a capitalised name, which `grace` is not once renamed.
    fixture.add_v2();
    let (code, message) = refused(&runtime, &push());
    assert_eq!(code, "ESCHEMA", "{message}");
    assert!(message.contains(OTHER_ID), "{message}");
    assert!(!message.contains(ID), "{message}");
    assert_eq!(fixture.head(), Some(1), "a refused push commits nothing");
    assert!(!fixture.registry_file(2).exists());

    // Once the document is fixed, the dry run passes and reports its scope.
    success(
        &runtime,
        &json!({"op":"patchDoc","collection":PEOPLE,"id":OTHER_ID,"newDoc":{"fullName":"Grace"}}),
    );
    let report = success(&runtime, &push());
    assert_eq!(labels(&report), ["v2"], "{report}");
    assert_eq!(report["dryRun"]["checked"], 2, "{report}");
    assert_eq!(report["dryRun"]["total"], 2, "{report}");
}

#[test]
fn the_registry_answers_once_the_folder_is_gone() {
    let fixture = Fixture::new();
    fixture.add_v2();
    let runtime = fixture.runtime();
    success(&runtime, &push());
    fs::remove_dir_all(&fixture.schema).unwrap();

    // No folder is configured at all now.
    let bare = fixture.runtime_with(&Options {
        schema: false,
        ..Options::default()
    });
    let (code, _) = refused(&bare, &put(ID, &json!({"name":"lower"})));
    assert_eq!(code, "ESCHEMA");
    success(&bare, &put(ID, &json!({"name":"Ada","ssn":"123"})));
    let stored = fixture.stored(ID);
    assert_eq!(stored["_v"], "v2", "{stored}");
    assert!(
        stored["ssn"]
            .as_str()
            .is_some_and(|text| text.starts_with("v2.")),
        "the head's $encrypted list applies: {stored}"
    );
    let document = get(&bare, ID);
    assert_eq!(document[ID]["ssn"], "123", "{document}");

    let inspect = success(&bare, &json!({"op":"schemaInspect","collection":PEOPLE}));
    assert_eq!(inspect["source"], "registry", "{inspect}");
    assert_eq!(inspect["current"], "v2", "{inspect}");
    let materialized = success(
        &bare,
        &json!({
            "op":"schemaMaterialize",
            "collection":PEOPLE,
            "document":{"_v":"v1","fullName":"Grace"}
        }),
    );
    assert_eq!(
        materialized["document"],
        json!({"_v":"v2","name":"Grace"}),
        "{materialized}"
    );
    let validated = success(
        &bare,
        &json!({"op":"schemaValidate","collection":PEOPLE,"document":{"name":"Grace"}}),
    );
    assert_eq!(validated["document"]["_v"], "v2", "{validated}");
    let doctor = success(&bare, &json!({"op":"schemaDoctor","collection":PEOPLE}));
    assert_eq!(doctor["ok"], true, "{doctor}");
}

#[test]
fn documents_decrypt_and_migrate_at_their_own_registry_version() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime();
    success(&runtime, &push());
    success(&runtime, &put(ID, &json!({"fullName":"Ada","ssn":"42"})));
    fixture.add_v2();
    success(&runtime, &push());
    fs::remove_dir_all(&fixture.schema).unwrap();

    let bare = fixture.runtime_with(&Options {
        schema: false,
        ..Options::default()
    });
    // A read returns the head shape, decrypted at v1 and migrated in
    // memory; the stored document stays at v1 until it is migrated.
    let document = get(&bare, ID);
    assert_eq!(
        document[ID],
        json!({"_v":"v2","name":"Ada","ssn":"42"}),
        "{document}"
    );
    assert_eq!(fixture.stored(ID)["_v"], "v1");
    assert!(fixture.stored(ID).get("fullName").is_some());
    let migrated = success(&bare, &json!({"op":"schemaMigrate","collection":PEOPLE}));
    assert_eq!(migrated["migrated"], 1, "{migrated}");
    assert_eq!(fixture.stored(ID)["_v"], "v2");
    let document = get(&bare, ID);
    assert_eq!(
        document[ID],
        json!({"_v":"v2","name":"Ada","ssn":"42"}),
        "{document}"
    );
}

#[test]
fn the_first_open_with_a_folder_imports_it_once() {
    let fixture = Fixture::unimported();
    // A session without a folder imports nothing.
    let bare = fixture.runtime_with(&Options {
        schema: false,
        ..Options::default()
    });
    success(&bare, &put(OTHER_ID, &json!({"fullName":"Grace"})));
    assert_eq!(fixture.head(), None);

    // The first session with a folder imports it, before its request.
    let runtime = fixture.runtime();
    let current = success(&runtime, &json!({"op":"schemaCurrent","collection":PEOPLE}));
    assert_eq!(current["current"], "v1", "{current}");
    assert_eq!(fixture.head(), Some(1));
    assert_eq!(
        fixture.version_record(1)["committed"]["actor"],
        "fylo.schema import"
    );
    let record = read_json(&fixture.root.join(".fylo-schemas/import.json"));
    assert_eq!(record["format"], "fylo.schema-import.v1", "{record}");
    assert_eq!(
        record["collections"][PEOPLE]["pushed"][0]["label"], "v1",
        "{record}"
    );

    // The import never runs again: a new folder version waits for a push.
    fixture.add_v2();
    let later = fixture.runtime();
    let current = success(&later, &json!({"op":"schemaCurrent","collection":PEOPLE}));
    assert_eq!(current["current"], "v1", "{current}");
    assert_eq!(fixture.head(), Some(1));
    // And the registry, not the folder, is what writes use meanwhile.
    success(&later, &put(ID, &json!({"fullName":"Ada"})));
    assert_eq!(fixture.stored(ID)["_v"], "v1");
}

#[test]
fn auto_push_commits_new_folder_versions_at_open() {
    let fixture = Fixture::new();
    let auto = Options {
        auto_push: true,
        ..Options::default()
    };
    success(
        &fixture.runtime_with(&auto),
        &json!({"op":"schemaCurrent","collection":PEOPLE}),
    );
    assert_eq!(fixture.head(), Some(1));

    fixture.add_v2();
    // Without autoPush the new version waits.
    success(&fixture.runtime(), &put(ID, &json!({"fullName":"Ada"})));
    assert_eq!(fixture.head(), Some(1));
    // With it, the next open pushes it, migrating `Ada` in the dry run.
    let runtime = fixture.runtime_with(&auto);
    success(&runtime, &put(OTHER_ID, &json!({"name":"Grace"})));
    assert_eq!(fixture.head(), Some(2));
    assert_eq!(
        fixture.version_record(2)["committed"]["actor"],
        "fylo.schema autoPush"
    );
    assert_eq!(fixture.stored(OTHER_ID)["_v"], "v2");
}

#[test]
fn push_and_pull_refuse_a_collection_they_cannot_serve() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime_with(&Options {
        schema: false,
        ..Options::default()
    });
    let (code, _) = refused(&runtime, &push());
    assert_eq!(code, "ESCHEMA", "no folder is configured");
    let (code, message) = refused(
        &runtime,
        &json!({"op":"schemaPull","collection":PEOPLE,"out":fixture.parent.join("out")}),
    );
    assert_eq!(code, "ESCHEMA", "{message}");
    assert!(message.contains("no schema registry"), "{message}");
    let (code, _) = refused(
        &fixture.runtime(),
        &json!({"op":"schemaPush","collection":"missing"}),
    );
    assert!(code.starts_with("ENATIVE_"), "{code}");
}
