//! The `fylo` crate opens `.vdbx` databases, and a protected one only with
//! an application identity and a trusted anchor (ADR 0021, phase 5).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo::{AccessTrust, Identity, KeyReference, LinkIdentity, OpenOptions, Root};
use fylo_storage_native::NativeRoot;
use serde_json::{Map, Value, json};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "fylo-crate-vdbx-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        Self(fs::canonicalize(directory).unwrap())
    }

    fn path(&self) -> PathBuf {
        self.0.join("app.vdbx")
    }

    fn identity(&self, name: &str) -> Identity {
        Identity::create(&KeyReference::File(self.0.join(format!("{name}.pem")))).unwrap()
    }

    /// A protected database whose admin is `admin`.
    fn protected(&self) -> Identity {
        let admin = self.identity("admin");
        NativeRoot::create_protected_database(self.path(), &admin).unwrap();
        admin
    }

    fn as_identity(identity: &Identity, anchor: &Identity) -> OpenOptions {
        OpenOptions::default()
            .with_identity(reopened(identity), AccessTrust::Pinned(anchor.id().clone()))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.path());
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn reopened(identity: &Identity) -> Identity {
    Identity::open(identity.reference()).unwrap()
}

fn fields(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn round_trip(root: &Root) {
    root.create_collection("people").unwrap();
    let people = root.collection("people");
    let id = people.put(fields(&json!({"name": "Ada"}))).unwrap();
    assert_eq!(people.get(&id).unwrap().unwrap()["name"], "Ada");
}

#[test]
fn a_plain_database_opens_without_a_lease_and_round_trips() {
    let fixture = Fixture::new();
    NativeRoot::create_database(fixture.path()).unwrap();
    let root = Root::open(fixture.path(), OpenOptions::default()).unwrap();
    round_trip(&root);
    root.check_lease().unwrap();
    root.close().unwrap();
}

#[test]
fn a_protected_database_opens_with_its_admin() {
    let fixture = Fixture::new();
    let admin = fixture.protected();
    let root = Root::open(fixture.path(), Fixture::as_identity(&admin, &admin)).unwrap();
    round_trip(&root);
    root.close().unwrap();
}

#[test]
fn a_protected_database_without_an_identity_is_locked() {
    let fixture = Fixture::new();
    fixture.protected();
    let root = Root::open(fixture.path(), OpenOptions::default()).unwrap();
    let error = root.create_collection("people").unwrap_err();
    assert_eq!(error.code(), "ENOIDENTITY", "{error}");
    root.close().unwrap();
}

#[test]
fn another_anchor_is_refused() {
    let fixture = Fixture::new();
    let admin = fixture.protected();
    let other = fixture.identity("other");
    let error = Root::open(fixture.path(), Fixture::as_identity(&admin, &other)).unwrap_err();
    assert_eq!(error.code(), "EPOLICY", "{error}");
}

#[test]
fn a_database_holds_no_queue() {
    let fixture = Fixture::new();
    NativeRoot::create_database(fixture.path()).unwrap();
    let root = Root::open(fixture.path(), OpenOptions::default()).unwrap();
    let error = root
        .queue("jobs")
        .publish(json!({"job": 1}), &fylo::PublishOptions::default())
        .unwrap_err();
    assert_eq!(error.code(), "ELAYOUTUNSUPPORTED", "{error}");
    root.close().unwrap();
}

#[test]
fn the_link_hook_reads_the_identity_beside_the_database() {
    let fixture = Fixture::new();
    let admin = fixture.protected();
    let settings = json!({
        "fylo": {
            "identity": format!("file:{}", fixture.0.join("admin.pem").display()),
            "trustAnchor": admin.id().as_str(),
        }
    });
    fs::write(fixture.0.join("application.json"), settings.to_string()).unwrap();
    let options = LinkIdentity::open_options(&fixture.path()).unwrap();
    let root = Root::open(fixture.path(), options).unwrap();
    round_trip(&root);
    root.close().unwrap();
}

#[test]
fn the_link_hook_opens_without_an_identity_when_none_is_named() {
    let fixture = Fixture::new();
    NativeRoot::create_database(fixture.path()).unwrap();
    let options = LinkIdentity::open_options(&fixture.path()).unwrap();
    let root = Root::open(fixture.path(), options).unwrap();
    round_trip(&root);
    root.close().unwrap();
}

#[cfg(feature = "sql")]
fn names(rows: &fylo::SqlRows) -> Vec<fylo::SqlValue> {
    rows.rows.iter().map(|row| row[0].clone()).collect()
}

#[cfg(feature = "sql")]
fn text(value: &str) -> fylo::SqlValue {
    fylo::SqlValue::Text(value.to_owned())
}

#[cfg(feature = "sql")]
#[test]
fn sql_reads_and_writes_a_protected_database_as_its_identity() {
    let fixture = Fixture::new();
    let admin = fixture.protected();
    let root = std::sync::Arc::new(
        Root::open(fixture.path(), Fixture::as_identity(&admin, &admin)).unwrap(),
    );
    round_trip(&root);
    let sql = root.sql().unwrap();
    assert_eq!(sql.tables(), ["people"]);
    let inserted = sql
        .execute(
            "INSERT INTO people (doc) VALUES (?)",
            &[text(r#"{"name":"Grace"}"#)],
        )
        .unwrap();
    assert_eq!(inserted, 1);
    let rows = sql
        .query(
            "SELECT json_extract(doc, '$.name') AS name FROM people ORDER BY name",
            &[],
        )
        .unwrap();
    assert_eq!(names(&rows), [text("Ada"), text("Grace")]);
    drop(sql);
    std::sync::Arc::into_inner(root).unwrap().close().unwrap();

    let bytes = fs::read(fixture.path()).unwrap();
    assert!(!bytes.windows(5).any(|window| window == b"Grace"));
    let reopened = std::sync::Arc::new(
        Root::open(fixture.path(), Fixture::as_identity(&admin, &admin)).unwrap(),
    );
    let rows = reopened
        .sql()
        .unwrap()
        .query("SELECT count(*) FROM people", &[])
        .unwrap();
    assert_eq!(rows.rows[0][0], fylo::SqlValue::Integer(2));
}

#[cfg(feature = "sql")]
#[test]
fn sql_reads_and_writes_a_plain_database() {
    let fixture = Fixture::new();
    NativeRoot::create_database(fixture.path()).unwrap();
    let root = std::sync::Arc::new(Root::open(fixture.path(), OpenOptions::default()).unwrap());
    round_trip(&root);
    let sql = root.sql().unwrap();
    sql.execute(
        "UPDATE people SET doc = json_set(doc, '$.name', 'Ada Lovelace')",
        &[],
    )
    .unwrap();
    let rows = sql
        .query("SELECT json_extract(doc, '$.name') FROM people", &[])
        .unwrap();
    assert_eq!(names(&rows), [text("Ada Lovelace")]);
}

#[cfg(feature = "sql")]
#[test]
fn sql_over_a_protected_database_without_an_identity_is_refused() {
    let fixture = Fixture::new();
    fixture.protected();
    let root = std::sync::Arc::new(Root::open(fixture.path(), OpenOptions::default()).unwrap());
    let error = root.sql().err().unwrap();
    assert_eq!(error.code(), "ENOIDENTITY", "{error}");
}

#[cfg(feature = "sql")]
#[test]
fn an_identity_without_a_grant_opens_no_sql_connection() {
    let fixture = Fixture::new();
    let admin = fixture.protected();
    let root = Root::open(fixture.path(), Fixture::as_identity(&admin, &admin)).unwrap();
    round_trip(&root);
    root.close().unwrap();
    let stranger = fixture.identity("stranger");
    let error = Root::open(fixture.path(), Fixture::as_identity(&stranger, &admin))
        .err()
        .unwrap();
    assert!(error.to_string().contains("no grant"), "{error}");
}

#[cfg(feature = "sql")]
#[test]
fn the_link_hook_opens_a_protected_database_with_the_identity_beside_it() {
    let fixture = Fixture::new();
    let admin = fixture.protected();
    let root = Root::open(fixture.path(), Fixture::as_identity(&admin, &admin)).unwrap();
    round_trip(&root);
    root.close().unwrap();
    let settings = json!({
        "fylo": {
            "identity": format!("file:{}", fixture.0.join("admin.pem").display()),
            "trustAnchor": admin.id().as_str(),
        }
    });
    fs::write(fixture.0.join("application.json"), settings.to_string()).unwrap();
    assert!(fylo::is_fylo_root(&fixture.path()));
    assert!(!fylo::is_fylo_root(&fixture.0.join("plain.db")));
    fylo::install_turso_open_hook();
    let database = turso_open(&fixture.path()).unwrap();
    let rows = database
        .connect()
        .unwrap()
        .prepare("SELECT json_extract(doc, '$.name') FROM people")
        .unwrap()
        .run_collect_rows()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(format!("{:?}", rows[0][0]).contains("Ada"), "{rows:?}");
}

#[cfg(feature = "sql")]
#[test]
fn the_link_hook_refuses_a_missing_database_instead_of_creating_one() {
    let fixture = Fixture::new();
    fylo::install_turso_open_hook();
    let error = turso_open(&fixture.path()).err().unwrap();
    assert!(error.to_string().contains("ENOENT"), "{error}");
    assert!(!fixture.path().exists());
}

#[cfg(feature = "sql")]
fn turso_open(path: &std::path::Path) -> turso_core::Result<std::sync::Arc<turso_core::Database>> {
    let io: std::sync::Arc<dyn turso_core::IO> =
        std::sync::Arc::new(turso_core::PlatformIO::new().unwrap());
    turso_core::Database::open_file(
        io,
        path.to_str().unwrap(),
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
}
