//! Lazy migration and convergence through the machine protocol (ADR 0017
//! sections 5 and 6): a document stored below its collection's head reads in
//! the head shape, a query or SQL statement on a migrated field matches it,
//! a write stores the head shape, and `schemaInspect` reports how many
//! documents are still to converge, whether the versions come from the
//! registry or from a `fylo.schema` folder.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, InvocationOptions, InvocationRuntime};
use fylo_storage_native::{NativeRoot, RootConfig, RootOpenOptions};
use serde_json::{Value, json};

const ADA: &str = "4VRNF52JPCO";
const GRACE: &str = "4VRNF52JPCP";
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

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    schema: PathBuf,
}

impl Fixture {
    /// A root with `people`, created by a session whose `fylo.schema`
    /// folder has `v1` only, holding Ada and Grace at `v1`.
    fn new() -> Self {
        Self::with_layout("files")
    }

    /// As [`Self::new`], with `people` in `layout` (ADR 0015).
    fn with_layout(layout: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-machine-lazy-{}-{stamp}-{}",
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
        let runtime = fixture.runtime();
        success(
            &runtime,
            &json!({"op":"createCollection","collection":PEOPLE,"layout":layout}),
        );
        fixture
    }

    fn put_both(runtime: &InvocationRuntime) {
        success(
            runtime,
            &put(ADA, &json!({"fullName":"Ada Lovelace","ssn":"42"})),
        );
        success(runtime, &put(GRACE, &json!({"fullName":"Grace Hopper"})));
    }

    fn write_version(
        &self,
        versions: &[&str],
        label: &str,
        schema: &Value,
        migration: Option<(&str, &Value)>,
    ) {
        let folder = self.schema.join(PEOPLE);
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

    /// A fresh invocation runtime: each one reopens the root.
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

fn put(id: &str, data: &Value) -> Value {
    json!({"op":"putData","collection":PEOPLE,"id":id,"data":data})
}

fn get(runtime: &InvocationRuntime, id: &str) -> Value {
    success(
        runtime,
        &json!({"op":"getDoc","collection":PEOPLE,"id":id,"onlyId":false}),
    )[id]
        .clone()
}

fn find(runtime: &InvocationRuntime, query: &Value) -> Value {
    success(
        runtime,
        &json!({"op":"findDocs","collection":PEOPLE,"query":query}),
    )
}

fn inspect(runtime: &InvocationRuntime) -> Value {
    success(runtime, &json!({"op":"schemaInspect","collection":PEOPLE}))
}

fn documents_per_version(report: &Value) -> Vec<u64> {
    report["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|version| version["documents"].as_u64().unwrap())
        .collect()
}

fn by_name(name: &str) -> Value {
    json!({"$ops":[{"name":{"$eq":name}}]})
}

#[test]
fn registry_documents_read_query_and_write_in_the_head_shape_until_they_converge() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime();
    success(&runtime, &json!({"op":"schemaPush","collection":PEOPLE}));
    Fixture::put_both(&runtime);
    fixture.add_v2();
    success(&runtime, &json!({"op":"schemaPush","collection":PEOPLE}));

    let runtime = fixture.runtime();
    let report = inspect(&runtime);
    assert_eq!(report["source"], "registry", "{report}");
    assert_eq!(report["head"], "v2", "{report}");
    assert!(report["validation"].is_string(), "{report}");
    assert_eq!(documents_per_version(&report), [2, 0], "{report}");
    assert_eq!(report["documents"], 2, "{report}");
    assert_eq!(report["unversioned"], 0, "{report}");
    assert_eq!(report["converged"], false, "{report}");

    // Reads return the head shape; storage keeps `v1`.
    assert_eq!(
        get(&runtime, ADA),
        json!({"name":"Ada Lovelace","ssn":"42","_v":"v2"})
    );
    assert_eq!(fixture.stored(ADA)["_v"], "v1");
    assert!(fixture.stored(ADA).get("fullName").is_some());
    let found = find(&runtime, &by_name("Ada Lovelace"));
    assert_eq!(
        found,
        json!({ADA: {"name":"Ada Lovelace","ssn":"42","_v":"v2"}}),
        "a query on the renamed field matches the stored v1 document"
    );
    assert_eq!(
        find(
            &runtime,
            &json!({"$ops":[{"fullName":{"$eq":"Ada Lovelace"}}]})
        ),
        json!({})
    );
    let selected = success(
        &runtime,
        &json!({"op":"executeSQL","sql":"SELECT * FROM people WHERE name = 'Grace Hopper'"}),
    );
    assert!(selected.to_string().contains("Grace Hopper"), "{selected}");

    // SQL UPDATE matches the migrated field, and its write stores the head
    // shape with the encrypted field still encrypted.
    let updated = success(
        &runtime,
        &json!({"op":"executeSQL","sql":"UPDATE people SET ssn = '7' WHERE name = 'Grace Hopper'"}),
    );
    assert_eq!(updated, 1);
    let stored = fixture.stored(GRACE);
    assert_eq!(stored["_v"], "v2", "{stored}");
    assert_eq!(stored["name"], "Grace Hopper", "{stored}");
    assert!(stored.get("fullName").is_none(), "{stored}");
    assert_ne!(stored["ssn"], "7", "ssn is stored encrypted: {stored}");
    assert_eq!(
        get(&runtime, GRACE),
        json!({"name":"Grace Hopper","ssn":"7","_v":"v2"})
    );
    let report = inspect(&runtime);
    assert_eq!(documents_per_version(&report), [1, 1], "{report}");
    assert_eq!(report["converged"], false, "{report}");

    // A patch migrates the stored document before applying its change.
    success(
        &runtime,
        &json!({"op":"patchDoc","collection":PEOPLE,"id":ADA,"newDoc":{"ssn":"43"}}),
    );
    let stored = fixture.stored(ADA);
    assert_eq!(stored["_v"], "v2", "{stored}");
    assert_eq!(stored["name"], "Ada Lovelace", "{stored}");
    assert!(stored.get("fullName").is_none(), "{stored}");
    let report = inspect(&runtime);
    assert_eq!(documents_per_version(&report), [0, 2], "{report}");
    assert_eq!(report["converged"], true, "{report}");
    assert_eq!(
        find(&runtime, &by_name("Ada Lovelace")),
        json!({ADA: {"name":"Ada Lovelace","ssn":"43","_v":"v2"}})
    );

    // SQL `WHERE` compares decrypted values, not ciphertext.
    let deleted = success(
        &runtime,
        &json!({"op":"executeSQL","sql":"DELETE FROM people WHERE ssn = '43'"}),
    );
    assert_eq!(deleted, 1);
    let report = inspect(&runtime);
    assert_eq!(documents_per_version(&report), [0, 1], "{report}");
    assert_eq!(report["converged"], true, "{report}");
}

#[test]
fn folder_documents_read_in_the_head_shape_and_converge_by_migration() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime();
    Fixture::put_both(&runtime);
    fixture.add_v2();

    let runtime = fixture.runtime();
    let report = inspect(&runtime);
    assert!(report["manifestPath"].is_string(), "{report}");
    assert_eq!(report["head"], "v2", "{report}");
    assert_eq!(documents_per_version(&report), [2, 0], "{report}");
    assert_eq!(report["converged"], false, "{report}");
    assert_eq!(
        get(&runtime, ADA),
        json!({"name":"Ada Lovelace","ssn":"42","_v":"v2"})
    );
    assert_eq!(fixture.stored(ADA)["_v"], "v1");
    assert_eq!(
        find(&runtime, &by_name("Grace Hopper")),
        json!({GRACE: {"name":"Grace Hopper","_v":"v2"}})
    );

    let migrated = success(&runtime, &json!({"op":"schemaMigrate","collection":PEOPLE}));
    assert_eq!(migrated["migrated"], 2, "{migrated}");
    assert_eq!(fixture.stored(ADA)["_v"], "v2");
    let report = inspect(&runtime);
    assert_eq!(documents_per_version(&report), [0, 2], "{report}");
    assert_eq!(report["converged"], true, "{report}");
    assert_eq!(
        get(&runtime, ADA),
        json!({"name":"Ada Lovelace","ssn":"42","_v":"v2"})
    );
}

#[test]
fn a_segment_collection_reads_at_the_head_and_is_counted_by_scan() {
    let fixture = Fixture::with_layout("segments");
    let runtime = fixture.runtime();
    success(&runtime, &json!({"op":"schemaPush","collection":PEOPLE}));
    Fixture::put_both(&runtime);
    fixture.add_v2();
    success(&runtime, &json!({"op":"schemaPush","collection":PEOPLE}));

    let runtime = fixture.runtime();
    let report = inspect(&runtime);
    assert_eq!(report["head"], "v2", "{report}");
    assert_eq!(documents_per_version(&report), [2, 0], "{report}");
    assert_eq!(report["converged"], false, "{report}");
    assert_eq!(
        get(&runtime, ADA),
        json!({"name":"Ada Lovelace","ssn":"42","_v":"v2"})
    );
    assert_eq!(
        find(&runtime, &by_name("Grace Hopper")),
        json!({GRACE: {"name":"Grace Hopper","_v":"v2"}}),
        "a query on the renamed field matches the stored v1 document"
    );

    success(
        &runtime,
        &json!({"op":"patchDoc","collection":PEOPLE,"id":ADA,"newDoc":{"ssn":"43"}}),
    );
    let report = inspect(&runtime);
    assert_eq!(documents_per_version(&report), [1, 1], "{report}");
    let migrated = success(&runtime, &json!({"op":"schemaMigrate","collection":PEOPLE}));
    assert_eq!(migrated["migrated"], 1, "{migrated}");
    let report = inspect(&runtime);
    assert_eq!(documents_per_version(&report), [0, 2], "{report}");
    assert_eq!(report["converged"], true, "{report}");
    assert_eq!(
        find(&runtime, &by_name("Grace Hopper")),
        json!({GRACE: {"name":"Grace Hopper","_v":"v2"}})
    );

    // Segment collections are counted by scan; the writer keeps no file. Their
    // registry is in schema records, so `.fylo-schemas` may not exist at all.
    let registry = fixture.root.join(".fylo-schemas").join(PEOPLE);
    let files: Vec<String> = fs::read_dir(registry)
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("counts"))
        .collect();
    assert!(files.is_empty(), "{files:?}");
}
