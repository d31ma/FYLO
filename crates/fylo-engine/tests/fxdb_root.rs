//! The engine over a `.fxdb` database root (ADR 0016 section 11): writes
//! through `WriteEngine`, then reads, prefix-index queries and a reopen
//! through `ReadOnlyEngine`, with nothing beside the file but its lease.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_engine::{ReadOnlyEngine, WriteEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{CollectionKind, NativeRoot, NativeWriteRoot, WriteAccess};
use serde_json::{Map, Value, json};

struct Database(PathBuf);

impl Database {
    fn create() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("fylo-engine-fxdb-{}-{nonce}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let directory = fs::canonicalize(directory).unwrap();
        NativeRoot::create_database(directory.join("db.fxdb")).unwrap();
        Self(directory)
    }

    fn path(&self) -> PathBuf {
        self.0.join("db.fxdb")
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.path());
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 30_000 + number)
}

fn fields(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn find(engine: &ReadOnlyEngine, collection: &str, query: &Value) -> Vec<String> {
    let query = StructuredQuery::from_value(query, QueryLimits::default()).unwrap();
    let mut found: Vec<String> = engine
        .find(collection, &query)
        .unwrap()
        .into_iter()
        .map(|row| row.document.fields()["name"].as_str().unwrap().to_owned())
        .collect();
    found.sort();
    found
}

#[test]
fn the_engine_writes_finds_and_reopens_a_database() {
    let database = Database::create();
    let writer = NativeWriteRoot::open(database.path()).unwrap();
    for name in ["users", "orders"] {
        writer
            .create_collection(name, CollectionKind::Document, None)
            .unwrap();
    }
    let engine = WriteEngine::open(database.path()).unwrap();
    for number in 0..9 {
        let team = ["red", "blue", "green"][number % 3];
        let user = json!({ "name": format!("user{number}"), "team": team });
        engine
            .put_document(
                "users",
                &identifier(number),
                fields(&user),
                WriteAccess::default(),
            )
            .unwrap();
    }
    engine
        .put_document(
            "orders",
            &identifier(100),
            fields(&json!({ "name": "order100", "user": identifier(1) })),
            WriteAccess::default(),
        )
        .unwrap();
    engine
        .patch_document(
            "users",
            &identifier(0),
            fields(&json!({ "name": "user0", "team": "blue" })),
            None,
        )
        .unwrap();

    let reader = ReadOnlyEngine::open(database.path()).unwrap();
    assert_eq!(
        reader
            .get("users", &identifier(4))
            .unwrap()
            .document
            .fields()["team"],
        "blue"
    );
    let blue = json!({"$ops":[{"team":{"$eq":"blue"}}]});
    assert_eq!(
        find(&reader, "users", &blue),
        ["user0", "user1", "user4", "user7"]
    );
    assert_eq!(
        find(
            &reader,
            "orders",
            &json!({"$ops":[{"user":{"$eq":identifier(1)}}]})
        ),
        ["order100"]
    );

    drop((writer, engine, reader));
    NativeRoot::close_database(database.path()).unwrap();

    let reader = ReadOnlyEngine::open(database.path()).unwrap();
    assert_eq!(
        find(&reader, "users", &blue),
        ["user0", "user1", "user4", "user7"]
    );
    assert_eq!(
        find(
            &reader,
            "users",
            &json!({"$ops":[{"team":{"$eq":"green"}}]})
        ),
        ["user2", "user5", "user8"]
    );
    assert_eq!(
        reader
            .get("orders", &identifier(100))
            .unwrap()
            .document
            .fields()["name"],
        "order100"
    );
    let names: Vec<String> = fs::read_dir(&database.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .all(|name| name == "db.fxdb" || name.starts_with("db.fxdb.lease.")),
        "{names:?}"
    );
}
