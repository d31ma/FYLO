//! `schemaMigrate` rewrites documents below the head schema version at the
//! head, through the same validation and encryption as any write (ADR 0017
//! section 5, ADR 0018).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, InvocationOptions, InvocationRuntime};
use fylo_storage_native::{NativeRoot, RootConfig, RootOpenOptions};
use serde_json::{Value, json};

const PEOPLE: &str = "people";
const ADA: &str = "4VRNF52JPCO";
const GRACE: &str = "4VRNF52JPCP";
const LEGACY: &str = "4VRNF52JPCQ";
const KEY: &str = "fixture-key-at-least-thirty-two-bytes";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    schema: PathBuf,
}

impl Fixture {
    /// A root whose `people` collection is at v1, `{fullName, active?}`.
    fn new(encrypted: bool) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-schema-migrate-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let fixture = Self {
            root: parent.join("root"),
            schema: parent.join("schemas"),
            parent,
        };
        fs::create_dir_all(&fixture.root).unwrap();
        fs::create_dir_all(fixture.collection().join("history")).unwrap();
        fs::create_dir_all(fixture.collection().join("migrations")).unwrap();
        let mut v1 = json!({"fullName":"^.+$","active?":"^(true|false)$"});
        if encrypted {
            v1["$encrypted"] = json!(["fullName"]);
        }
        fixture.version("v1", &v1);
        fixture.manifest(&["v1"]);
        fixture
    }

    fn collection(&self) -> PathBuf {
        self.schema.join(PEOPLE)
    }

    fn manifest(&self, versions: &[&str]) {
        let entries: Vec<Value> = versions.iter().map(|v| json!({ "v": v })).collect();
        fs::write(
            self.collection().join("manifest.json"),
            serde_json::to_vec(&json!({"current":versions.last(),"versions":entries})).unwrap(),
        )
        .unwrap();
    }

    fn version(&self, label: &str, schema: &Value) {
        fs::write(
            self.collection()
                .join("history")
                .join(format!("{label}.schema.json")),
            serde_json::to_vec(schema).unwrap(),
        )
        .unwrap();
    }

    /// Add v2, `{name, slug, active?}`, reached by renaming `fullName` and
    /// slugifying it.
    fn add_v2(&self, encrypted: bool) {
        let mut v2 = json!({"name":"^.+$","slug":"^[a-z0-9-]+$","active?":"^(true|false)$"});
        if encrypted {
            v2["$encrypted"] = json!(["name"]);
        }
        self.version("v2", &v2);
        fs::write(
            self.collection().join("migrations/v1-to-v2.json"),
            br#"{"steps":[{"rename":"fullName","to":"name"},{"slugify":"slug","from":"name"}]}"#,
        )
        .unwrap();
        self.manifest(&["v1", "v2"]);
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

fn put(runtime: &InvocationRuntime, identifier: &str, data: &Value) {
    success(
        runtime,
        &json!({"op":"putData","collection":PEOPLE,"id":identifier,"data":data}),
    );
}

fn migrate(runtime: &InvocationRuntime, include_unversioned: bool) -> Value {
    let mut request = json!({"op":"schemaMigrate","collection":PEOPLE});
    if include_unversioned {
        request["includeUnversioned"] = json!(true);
    }
    let mut result = success(runtime, &request);
    result.as_object_mut().unwrap().remove("schemaDir");
    result
}

fn counts(scanned: usize, migrated: usize, at_head: usize, unversioned: usize) -> Value {
    json!({
        "collection": PEOPLE, "current": "v2", "scanned": scanned,
        "migrated": migrated, "atHead": at_head, "unversioned": unversioned
    })
}

/// Two v1 documents written in strict mode, which stamps `_v`, and one
/// written before strict mode, which has none.
fn seeded(encrypted: bool) -> Fixture {
    let fixture = Fixture::new(encrypted);
    let loose = fixture.runtime(false, encrypted);
    success(
        &loose,
        &json!({"op":"createCollection","collection":PEOPLE}),
    );
    put(&loose, LEGACY, &json!({"fullName":"Alan Turing"}));
    let strict = fixture.runtime(true, encrypted);
    put(
        &strict,
        ADA,
        &json!({"fullName":"Ada Lovelace","active":true}),
    );
    put(&strict, GRACE, &json!({"fullName":"Grace Hopper"}));
    fixture
}

#[test]
fn migrate_rewrites_old_documents_at_the_head_and_is_idempotent() {
    let fixture = seeded(false);
    fixture.add_v2(false);
    let runtime = fixture.runtime(true, false);
    assert_eq!(migrate(&runtime, false), counts(3, 2, 0, 1));
    assert_eq!(
        fixture.stored(ADA),
        json!({"name":"Ada Lovelace","slug":"ada-lovelace","active":true,"_v":"v2"})
    );
    assert_eq!(
        fixture.stored(GRACE),
        json!({"name":"Grace Hopper","slug":"grace-hopper","_v":"v2"})
    );
    assert_eq!(fixture.stored(LEGACY), json!({"fullName":"Alan Turing"}));
    assert_eq!(migrate(&runtime, false), counts(3, 0, 2, 1));
}

#[test]
fn migrate_takes_unversioned_documents_as_the_oldest_only_when_asked() {
    let fixture = seeded(false);
    fixture.add_v2(false);
    let runtime = fixture.runtime(true, false);
    assert_eq!(migrate(&runtime, true), counts(3, 3, 0, 0));
    assert_eq!(
        fixture.stored(LEGACY),
        json!({"name":"Alan Turing","slug":"alan-turing","_v":"v2"})
    );
}

#[test]
fn migrate_outside_strict_mode_keeps_the_head_stamp() {
    let fixture = seeded(false);
    fixture.add_v2(false);
    let runtime = fixture.runtime(false, false);
    assert_eq!(migrate(&runtime, false), counts(3, 2, 0, 1));
    assert_eq!(fixture.stored(GRACE)["_v"], "v2");
    assert_eq!(migrate(&runtime, false), counts(3, 0, 2, 1));
}

#[test]
fn migrate_decrypts_and_re_encrypts_declared_fields() {
    let fixture = seeded(true);
    fixture.add_v2(true);
    let runtime = fixture.runtime(true, true);
    assert_eq!(migrate(&runtime, false), counts(3, 2, 0, 1));
    let stored = fixture.stored(ADA);
    assert!(
        stored["name"].as_str().unwrap().starts_with("v2."),
        "{stored}"
    );
    assert!(stored.get("fullName").is_none(), "{stored}");
    assert_eq!(stored["slug"], "ada-lovelace");
    let read = success(
        &runtime,
        &json!({"op":"getDoc","collection":PEOPLE,"id":ADA}),
    );
    assert_eq!(read[ADA]["name"], "Ada Lovelace");
}

#[test]
fn an_old_document_decrypts_with_the_list_of_the_version_it_was_written_at() {
    let fixture = seeded(true);
    fixture.add_v2(true);
    let runtime = fixture.runtime(true, true);
    let read = success(
        &runtime,
        &json!({"op":"getDoc","collection":PEOPLE,"id":ADA}),
    );
    assert_eq!(read[ADA]["fullName"], "Ada Lovelace", "{read}");
    assert_eq!(migrate(&runtime, true), counts(3, 3, 0, 0));
    let read = success(
        &runtime,
        &json!({"op":"getDoc","collection":PEOPLE,"id":LEGACY}),
    );
    assert_eq!(
        read[LEGACY],
        json!({"name":"Alan Turing","slug":"alan-turing","_v":"v2"})
    );
}

#[test]
fn migrate_stops_at_a_document_the_head_rejects_and_leaves_it() {
    let fixture = seeded(false);
    fixture.add_v2(false);
    // v2 now refuses a name that is not two capitalised words.
    fixture.version(
        "v2",
        &json!({"name":"^[A-Z][a-z]+ [A-Z][a-z]+$","slug":"^[a-z0-9-]+$","active?":"^(true|false)$"}),
    );
    let runtime = fixture.runtime(false, false);
    put(
        &runtime,
        "4VRNF52JPCR",
        &json!({"fullName":"lowercase name","_v":"v1"}),
    );
    let strict = fixture.runtime(true, false);
    let response = execute(&strict, &json!({"op":"schemaMigrate","collection":PEOPLE}));
    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(response["error"]["code"], "ESCHEMA", "{response}");
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.contains("document 4VRNF52JPCR"), "{message}");
    assert_eq!(
        fixture.stored("4VRNF52JPCR"),
        json!({"fullName":"lowercase name","_v":"v1"})
    );
}

#[test]
fn migrate_needs_a_versioned_schema() {
    let fixture = seeded(false);
    let runtime = fixture.runtime(true, false);
    let response = execute(
        &runtime,
        &json!({"op":"schemaMigrate","collection":"unknown"}),
    );
    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(response["error"]["code"], "ESCHEMA", "{response}");
}
