//! The engine over a `.fxdb` database root (ADR 0016 section 11): writes
//! through `WriteEngine`, then reads, prefix-index queries and a reopen
//! through `ReadOnlyEngine`, with nothing beside the file but its lease, and
//! schema versions pushed into the file's registry (ADR 0017 section 3). An
//! encrypted database opens with its key, and is locked without it (ADR
//! 0016 section 9).

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_engine::{EngineErrorCode, ReadOnlyEngine, WriteEngine};
use fylo_format::ContainerSuperblock;
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{
    CollectionKind, ContainerFile, DatabaseKey, NativeRoot, NativeStorageErrorCode,
    NativeWriteRoot, RootConfig, RootOpenOptions, WriteAccess,
};
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

/// Write version `label` of `collection` into the `fylo.schema` folder at
/// `folder`, with the manifest listing `versions` and, for a version after
/// the first, the migration from the one before.
fn author(
    folder: &std::path::Path,
    (collection, versions): (&str, &[&str]),
    (label, chex): (&str, &Value),
    migration: Option<(&str, &Value)>,
) {
    let folder = folder.join(collection);
    fs::create_dir_all(folder.join("history")).unwrap();
    fs::create_dir_all(folder.join("migrations")).unwrap();
    let entries: Vec<Value> = versions.iter().map(|v| json!({ "v": v })).collect();
    fs::write(
        folder.join("manifest.json"),
        serde_json::to_vec(&json!({ "current": versions.last(), "versions": entries })).unwrap(),
    )
    .unwrap();
    fs::write(
        folder.join("history").join(format!("{label}.schema.json")),
        serde_json::to_vec(chex).unwrap(),
    )
    .unwrap();
    if let Some((from, steps)) = migration {
        fs::write(
            folder
                .join("migrations")
                .join(format!("{from}-to-{label}.json")),
            serde_json::to_vec(steps).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn the_engine_pushes_and_validates_schema_versions_in_a_database() {
    let database = Database::create();
    let schemas = database.0.join("schemas");
    NativeWriteRoot::open(database.path())
        .unwrap()
        .create_collection("users", CollectionKind::Document, None)
        .unwrap();
    let engine = |schema_folder: bool| {
        let engine = WriteEngine::open(database.path())
            .unwrap()
            .with_config(RootConfig {
                strict_schema: true,
                ..RootConfig::default()
            });
        if schema_folder {
            engine.with_schema_configuration(&schemas, None).unwrap()
        } else {
            engine
        }
    };
    let put = |engine: &WriteEngine, number: usize, body: Value| {
        engine.put_document(
            "users",
            &identifier(number),
            fields(&body),
            WriteAccess::default(),
        )
    };

    author(
        &schemas,
        ("users", &["v1"]),
        ("v1", &json!({ "name": "^.+$", "team": "^(red|blue)$" })),
        None,
    );
    let writer = engine(true);
    writer.schema_push("users", None, None).unwrap();
    put(&writer, 0, json!({ "name": "user0", "team": "red" })).unwrap();
    let refused = put(&writer, 1, json!({ "name": "user1", "team": "purple" })).unwrap_err();
    assert_eq!(refused.code(), EngineErrorCode::Schema, "{refused}");

    // The second version renames a field; documents written under the
    // first read in the head's shape.
    author(
        &schemas,
        ("users", &["v1", "v2"]),
        ("v2", &json!({ "name": "^.+$", "squad": "^(red|blue)$" })),
        Some((
            "v1",
            &json!({ "steps": [{ "rename": "team", "to": "squad" }] }),
        )),
    );
    writer.schema_push("users", None, None).unwrap();
    put(&writer, 2, json!({ "name": "user2", "squad": "blue" })).unwrap();
    let refused = put(&writer, 3, json!({ "name": "user3", "team": "blue" })).unwrap_err();
    assert_eq!(refused.code(), EngineErrorCode::Schema, "{refused}");
    assert_eq!(
        writer.schema_current("users").unwrap().as_deref(),
        Some("v2")
    );
    drop(writer);
    NativeRoot::close_database(database.path()).unwrap();

    // Reopened, the file alone holds the registry: no schema folder is
    // needed to read the head or to validate against it.
    assert_eq!(
        NativeRoot::open(database.path())
            .unwrap()
            .schema_registry_head("users")
            .unwrap(),
        Some(2)
    );
    let file = ContainerFile::open_reader(&database.path()).unwrap();
    assert_eq!(
        file.superblock().read_only_compat & ContainerSuperblock::READ_ONLY_COMPAT_SCHEMAS,
        ContainerSuperblock::READ_ONLY_COMPAT_SCHEMAS
    );
    drop(file);
    let reader = ReadOnlyEngine::open(database.path()).unwrap();
    let first = reader.get("users", &identifier(0)).unwrap().document;
    assert_eq!(first.fields()["squad"], "red");
    assert!(first.fields().get("team").is_none());
    assert_eq!(
        reader
            .get("users", &identifier(2))
            .unwrap()
            .document
            .fields()["squad"],
        "blue"
    );
    let writer = engine(false);
    assert_eq!(
        writer.schema_current("users").unwrap().as_deref(),
        Some("v2")
    );
    let refused = put(&writer, 4, json!({ "name": "user4", "squad": "green" })).unwrap_err();
    assert_eq!(refused.code(), EngineErrorCode::Schema, "{refused}");
    put(&writer, 4, json!({ "name": "user4", "squad": "red" })).unwrap();
}

#[test]
fn the_engine_reads_an_encrypted_database_only_with_its_key() {
    let database = Database::create();
    let path = database.0.join("sealed.fxdb");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // A key generated for this test; no real secret is written.
    let key = DatabaseKey::new(format!("generated-engine-{stamp:032x}"));
    NativeRoot::create_encrypted_database(&path, &key).unwrap();
    let keyed = || RootOpenOptions::new(&path).with_database_key(key.clone());
    NativeWriteRoot::open_profiled(keyed())
        .unwrap()
        .create_collection("users", CollectionKind::Document, None)
        .unwrap();
    let engine = WriteEngine::open_profiled(keyed()).unwrap();
    for number in 0..4 {
        let team = ["red", "blue"][number % 2];
        engine
            .put_document(
                "users",
                &identifier(number),
                fields(&json!({ "name": format!("ECHIDNA{number}"), "team": team })),
                WriteAccess::default(),
            )
            .unwrap();
    }
    drop(engine);
    NativeRoot::close_database(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert!(!bytes.windows(7).any(|window| window == b"ECHIDNA"));

    let reader = ReadOnlyEngine::open_profiled(keyed()).unwrap();
    let blue = json!({"$ops":[{"team":{"$eq":"blue"}}]});
    assert_eq!(find(&reader, "users", &blue), ["ECHIDNA1", "ECHIDNA3"]);
    drop(reader);
    NativeRoot::close_database(&path).unwrap();

    // Without the key the database is locked, and nothing is written.
    let locked = ReadOnlyEngine::open(&path).unwrap();
    let refused = locked.get("users", &identifier(1)).unwrap_err();
    assert_eq!(
        refused.storage_code(),
        Some(NativeStorageErrorCode::KeyRequired),
        "{refused}"
    );
    let writer = WriteEngine::open(&path).unwrap();
    let refused = writer
        .put_document(
            "users",
            &identifier(9),
            fields(&json!({ "name": "late" })),
            WriteAccess::default(),
        )
        .unwrap_err();
    assert_eq!(
        refused.storage_code(),
        Some(NativeStorageErrorCode::KeyRequired),
        "{refused}"
    );
    drop((locked, writer));
    NativeRoot::close_database(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
}
