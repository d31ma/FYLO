//! Reads return every document in its collection's head shape, migrating an
//! old one in memory without changing what is stored, and a query over a
//! collection that has not converged answers as it would over the migrated
//! documents (ADR 0017 sections 5 and 6).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_engine::{HotSet, ReadOnlyEngine, WriteEngine};
use fylo_query::{JoinSpec, QueryLimits, StructuredQuery, prepare_sql};
use fylo_storage_native::{CollectionKind, NativeRoot, NativeWriteRoot, WriteAccess};
use serde_json::{Map, Value, json};

const PEOPLE: &str = "people";
const CITIES: &str = "cities";
const ADA: &str = "4VRNF52JPCO";
const GRACE: &str = "4VRNF52JPCP";
const ALAN: &str = "4VRNF52JPCQ";
const KEY: &str = "fixture-key-at-least-thirty-two-bytes";
const SALT: &str = "fixture-salt";

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    schema: PathBuf,
    encrypted: bool,
}

impl Fixture {
    /// A root whose `people` are at v1, `{fullName, city?, ssn?}`, with
    /// `ssn` encrypted when `encrypted`, and a `cities` collection to join.
    fn new(encrypted: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = std::env::temp_dir().join(format!(
            "fylo-lazy-migration-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&parent);
        let fixture = Self {
            root: parent.join("root"),
            schema: parent.join("schemas"),
            parent,
            encrypted,
        };
        fs::create_dir_all(&fixture.root).unwrap();
        fs::create_dir_all(fixture.folder().join("history")).unwrap();
        fs::create_dir_all(fixture.folder().join("migrations")).unwrap();
        let mut v1 = json!({"fullName":"^.+$","city?":"^.+$","ssn?":"^[0-9]+$"});
        if encrypted {
            v1["$encrypted"] = json!(["ssn"]);
        }
        fixture.version("v1", &v1);
        fixture.manifest(&["v1"]);
        let storage = NativeWriteRoot::open(&fixture.root).unwrap();
        storage
            .create_collection(PEOPLE, CollectionKind::Document, None)
            .unwrap();
        storage
            .create_collection(CITIES, CollectionKind::Document, None)
            .unwrap();
        fixture.writer().schema_push(PEOPLE, None, None).unwrap();
        fixture
    }

    fn folder(&self) -> PathBuf {
        self.schema.join(PEOPLE)
    }

    fn manifest(&self, versions: &[&str]) {
        let entries: Vec<Value> = versions.iter().map(|v| json!({ "v": v })).collect();
        fs::write(
            self.folder().join("manifest.json"),
            serde_json::to_vec(&json!({"current":versions.last(),"versions":entries})).unwrap(),
        )
        .unwrap();
    }

    fn version(&self, label: &str, schema: &Value) {
        fs::write(
            self.folder()
                .join("history")
                .join(format!("{label}.schema.json")),
            serde_json::to_vec(schema).unwrap(),
        )
        .unwrap();
    }

    /// v2 renames `fullName` to `name` and `ssn` to `taxId`, which stays
    /// encrypted.
    fn push_v2(&self) {
        let mut v2 = json!({"name":"^.+$","city?":"^.+$","taxId?":"^[0-9]+$"});
        if self.encrypted {
            v2["$encrypted"] = json!(["taxId"]);
        }
        self.version("v2", &v2);
        fs::write(
            self.folder().join("migrations/v1-to-v2.json"),
            br#"{"steps":[{"rename":"fullName","to":"name"},{"rename":"ssn","to":"taxId"}]}"#,
        )
        .unwrap();
        self.manifest(&["v1", "v2"]);
        self.writer().schema_push(PEOPLE, None, None).unwrap();
    }

    fn writer(&self) -> WriteEngine {
        WriteEngine::open(&self.root)
            .unwrap()
            .with_schema_configuration(&self.schema, self.encrypted.then_some((KEY, SALT)))
            .unwrap()
    }

    /// A reader with keys but no schema folder: the registry is enough.
    fn reader(&self) -> ReadOnlyEngine {
        let engine = ReadOnlyEngine::open(&self.root).unwrap();
        if self.encrypted {
            engine.with_encryption_credentials(KEY, SALT).unwrap()
        } else {
            engine
        }
    }

    fn put(&self, collection: &str, identifier: &str, data: &Value) {
        self.writer()
            .put_document(
                collection,
                identifier,
                data.as_object().unwrap().clone(),
                WriteAccess::default(),
            )
            .unwrap();
    }

    fn stored(&self, identifier: &str) -> Value {
        let stored = NativeRoot::open(&self.root)
            .unwrap()
            .collection(PEOPLE)
            .unwrap()
            .read_document(identifier)
            .unwrap();
        serde_json::from_slice(&stored.bytes).unwrap()
    }

    /// Ada and Grace at v1, then v2 pushed over them.
    fn seeded(encrypted: bool) -> Self {
        let fixture = Self::new(encrypted);
        fixture.put(
            PEOPLE,
            ADA,
            &json!({"fullName":"Ada Lovelace","city":"London","ssn":"111"}),
        );
        fixture.put(
            PEOPLE,
            GRACE,
            &json!({"fullName":"Grace Hopper","city":"New York","ssn":"222"}),
        );
        fixture.put(CITIES, ALAN, &json!({"city":"London","country":"UK"}));
        fixture.push_v2();
        fixture
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn query(value: &Value) -> StructuredQuery {
    StructuredQuery::from_value(value, QueryLimits::default()).unwrap()
}

fn ids(rows: &[fylo_engine::ReadDocument]) -> Vec<String> {
    rows.iter().map(|row| row.metadata.id.clone()).collect()
}

fn fields(row: &fylo_engine::ReadDocument) -> Map<String, Value> {
    row.document.fields().clone()
}

fn by_name(name: &str) -> StructuredQuery {
    query(&json!({"$ops":[{"name":{"$eq":name}}]}))
}

#[test]
fn every_read_path_returns_the_head_shape_and_leaves_storage_alone() {
    let fixture = Fixture::seeded(false);
    let stored = fixture.stored(ADA);
    let reader = fixture.reader();

    let ada = reader.get(PEOPLE, ADA).unwrap();
    assert_eq!(
        Value::Object(fields(&ada)),
        json!({"_v":"v2","name":"Ada Lovelace","city":"London","taxId":"111"})
    );
    let all = reader.find(PEOPLE, &query(&json!({}))).unwrap();
    assert_eq!(ids(&all), [ADA, GRACE]);
    assert!(all.iter().all(|row| fields(row).contains_key("name")));

    let plan = prepare_sql(
        "SELECT name FROM people WHERE name = 'Grace Hopper'",
        QueryLimits::default(),
    )
    .unwrap();
    assert_eq!(
        reader.select_sql(&plan).unwrap(),
        json!({ GRACE: {"name":"Grace Hopper"} })
    );

    let join = JoinSpec::from_value(
        &json!({
            "$leftCollection": PEOPLE,
            "$rightCollection": CITIES,
            "$mode": "inner",
            "$on": {"city": {"$eq": "city"}},
            "$select": ["name", "country"]
        }),
        QueryLimits::default(),
    )
    .unwrap();
    let joined = serde_json::to_value(reader.join(&join, None).unwrap()).unwrap();
    assert_eq!(
        joined,
        json!({ format!("{ADA}, {ALAN}"): {"name":"Ada Lovelace","country":"UK"} })
    );

    fixture
        .writer()
        .patch_document_fields(PEOPLE, GRACE, &Map::new(), None)
        .unwrap();
    NativeWriteRoot::open(&fixture.root)
        .unwrap()
        .delete_document(PEOPLE, ADA, None)
        .unwrap();
    let deleted = reader.get_deleted(PEOPLE, ADA).unwrap();
    assert_eq!(deleted.document.fields()["name"], "Ada Lovelace");

    // Nothing above rewrote Ada, who is still stored at v1.
    let tombstone = NativeRoot::open(&fixture.root)
        .unwrap()
        .collection(PEOPLE)
        .unwrap()
        .read_deleted_document(ADA)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&tombstone.bytes).unwrap(),
        stored
    );
    let as_stored = reader.as_stored().get_deleted(PEOPLE, ADA).unwrap();
    assert_eq!(as_stored.document.fields()["fullName"], "Ada Lovelace");
}

#[test]
fn a_query_on_a_renamed_field_answers_the_same_before_and_after_migration() {
    let fixture = Fixture::seeded(false);
    // A document written at v2 is indexed under `name` already.
    fixture.put(PEOPLE, ALAN, &json!({"name":"Alan Turing","city":"London"}));
    let hot_set = Arc::new(HotSet::new(HotSet::DEFAULT_BUDGET_BYTES));
    let check = |label: &str| {
        let cold = fixture.reader();
        let hot = fixture.reader().with_hot_set(Arc::clone(&hot_set));
        for (engine, kind) in [
            (&cold, "storage"),
            (&hot, "hot set"),
            (&hot, "warm hot set"),
        ] {
            assert_eq!(
                ids(&engine.find(PEOPLE, &by_name("Ada Lovelace")).unwrap()),
                [ADA],
                "{label}, {kind}"
            );
            assert_eq!(
                ids(&engine.find(PEOPLE, &by_name("Alan Turing")).unwrap()),
                [ALAN],
                "{label}, {kind}"
            );
            let old_name = query(&json!({"$ops":[{"fullName":{"$eq":"Ada Lovelace"}}]}));
            assert!(
                engine.find(PEOPLE, &old_name).unwrap().is_empty(),
                "{label}, {kind}"
            );
            // A field no migration touches keeps its index.
            let london = query(&json!({"$ops":[{"city":{"$eq":"London"}}]}));
            assert_eq!(
                ids(&engine.find(PEOPLE, &london).unwrap()),
                [ADA, ALAN],
                "{label}, {kind}"
            );
            let plan = prepare_sql(
                "SELECT * FROM people WHERE name = 'Ada Lovelace'",
                QueryLimits::default(),
            )
            .unwrap();
            let selected = engine.select_sql(&plan).unwrap();
            assert_eq!(selected[ADA]["name"], "Ada Lovelace", "{label}, {kind}");
        }
    };
    check("before migration");
    let writer = fixture.writer();
    for identifier in [ADA, GRACE] {
        writer.migrate_document(PEOPLE, identifier, None).unwrap();
    }
    hot_set.clear();
    assert_eq!(fixture.stored(ADA)["name"], "Ada Lovelace");
    check("after migration");
}

#[test]
fn encrypted_fields_decrypt_at_the_written_version_and_then_migrate() {
    let fixture = Fixture::seeded(true);
    let stored = fixture.stored(ADA);
    assert!(
        stored["ssn"].as_str().unwrap().starts_with("v2."),
        "{stored}"
    );
    let reader = fixture.reader();
    let ada = reader.get(PEOPLE, ADA).unwrap();
    assert_eq!(fields(&ada)["taxId"], "111");
    assert_eq!(fields(&ada)["name"], "Ada Lovelace");
    assert!(!fields(&ada).contains_key("ssn"));

    // `taxId` is encrypted at the head, so it is never planned from the
    // index; the scan decrypts and migrates each document.
    let by_tax = query(&json!({"$ops":[{"taxId":{"$eq":"222"}}]}));
    assert_eq!(ids(&reader.find(PEOPLE, &by_tax).unwrap()), [GRACE]);

    // A patch of an old document migrates its pre-image, so it is stored
    // at the head with the head's encryption list.
    fixture
        .writer()
        .patch_document_fields(
            PEOPLE,
            ADA,
            json!({"city":"Paris"}).as_object().unwrap(),
            None,
        )
        .unwrap();
    let patched = fixture.stored(ADA);
    assert_eq!(patched["_v"], "v2");
    assert!(patched.get("ssn").is_none(), "{patched}");
    assert!(
        patched["taxId"].as_str().unwrap().starts_with("v2."),
        "{patched}"
    );
    let ada = reader.get(PEOPLE, ADA).unwrap();
    assert_eq!(
        Value::Object(fields(&ada)),
        json!({"_v":"v2","name":"Ada Lovelace","city":"Paris","taxId":"111"})
    );
    // A lazy read and an eager migration give the same document.
    let lazy = reader.get(PEOPLE, GRACE).unwrap();
    fixture
        .writer()
        .migrate_document(PEOPLE, GRACE, None)
        .unwrap();
    assert_eq!(
        fixture.reader().get(PEOPLE, GRACE).unwrap().document,
        lazy.document
    );
}

#[test]
fn inspect_reports_documents_per_version_until_the_collection_converges() {
    let fixture = Fixture::seeded(false);
    let writer = fixture.writer();
    let report = writer.schema_inspect(PEOPLE).unwrap();
    assert_eq!(report["head"], "v2", "{report}");
    assert_eq!(report["validation"], "off", "{report}");
    assert_eq!(report["documents"], 2, "{report}");
    assert_eq!(report["converged"], false, "{report}");
    assert_eq!(report["versions"][0]["documents"], 2, "{report}");
    assert_eq!(report["versions"][1]["documents"], 0, "{report}");

    writer.migrate_document(PEOPLE, ADA, None).unwrap();
    let report = writer.schema_inspect(PEOPLE).unwrap();
    assert_eq!(report["versions"][0]["documents"], 1, "{report}");
    assert_eq!(report["versions"][1]["documents"], 1, "{report}");
    assert_eq!(report["converged"], false, "{report}");

    writer.migrate_document(PEOPLE, GRACE, None).unwrap();
    let report = writer.schema_inspect(PEOPLE).unwrap();
    assert_eq!(report["versions"][0]["documents"], 0, "{report}");
    assert_eq!(report["versions"][1]["documents"], 2, "{report}");
    assert_eq!(report["converged"], true, "{report}");

    // The writer kept the count at every commit; nothing had to recount.
    let collection = NativeRoot::open(&fixture.root)
        .unwrap()
        .collection(PEOPLE)
        .unwrap();
    let recorded = collection.recorded_schema_versions().unwrap().unwrap();
    assert_eq!(recorded, collection.scan_schema_versions().unwrap());
    assert_eq!(recorded.at("v2"), 2);
}
