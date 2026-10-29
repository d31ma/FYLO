use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fylo_storage_native::ContainerFile;
use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_fylo-rust");

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("fylo-cli-fxdb-{name}-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(BINARY)
            .current_dir(&self.0)
            .args(arguments)
            .output()
            .unwrap()
    }

    /// Run with `secret` as the database key `keyed.json` names, a
    /// configuration that does not encrypt new databases by itself.
    fn run_keyed(&self, secret: &str, arguments: &[&str]) -> Output {
        fs::write(
            self.0.join("keyed.json"),
            r#"{"fylo":{"encryption":{"key":"${FYLO_CLI_TEST_DATABASE_KEY}","file":true}}}"#,
        )
        .unwrap();
        Command::new(BINARY)
            .current_dir(&self.0)
            .args(arguments)
            .args(["--config", "keyed.json"])
            .env("FYLO_CLI_TEST_DATABASE_KEY", secret)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn database(path: &Path) {
    ContainerFile::create(path, 20).unwrap();
    let mut container = ContainerFile::open_unchecked(path).unwrap();
    container.add_collection("users", b"").unwrap();
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn inspect_prints_a_fxdb_report() {
    let fixture = Fixture::new("inspect");
    database(&fixture.0.join("app.fxdb"));
    let output = fixture.run(&["inspect", "app.fxdb"]);
    assert!(output.status.success(), "{output:?}");
    let report = json(&output);
    assert_eq!(report["format"], "fylo.fxdb");
    assert_eq!(report["collections"][0]["name"], "users");
    // No directory root was created in its place.
    assert!(!fixture.0.join(".fylo-data").exists());
}

#[test]
fn verify_prints_the_report_and_fails_on_damage() {
    let fixture = Fixture::new("verify");
    let path = fixture.0.join("app.fxdb");
    database(&path);
    let clean = fixture.run(&["verify", "app.fxdb"]);
    assert!(clean.status.success(), "{clean:?}");
    assert_eq!(json(&clean)["ok"], true);
    let by_root = fixture.run(&["verify", "--root", "app.fxdb"]);
    assert!(by_root.status.success(), "{by_root:?}");

    // Damage the directory extent's header.
    let mut bytes = fs::read(&path).unwrap();
    bytes[(1 << 20) + 20] ^= 0x40;
    fs::write(&path, bytes).unwrap();
    let damaged = fixture.run(&["verify", "app.fxdb"]);
    assert!(!damaged.status.success(), "{damaged:?}");
    let report = json(&damaged);
    assert_eq!(report["ok"], false);
    assert_eq!(report["problems"][0]["code"], "extentHeaderMismatch");

    let missing = fixture.run(&["verify", "missing.fxdb"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("ENATIVE_NOT_FOUND"));
}

#[test]
fn commands_and_sql_run_against_a_database_root() {
    let fixture = Fixture::new("root");
    let create = fixture.run(&[
        "exec",
        "--request",
        r#"{"op":"createCollection","collection":"users"}"#,
        "--root",
        "app.fxdb",
    ]);
    assert!(create.status.success(), "{create:?}");
    let insert = fixture.run(&[
        "sql",
        "INSERT INTO users (name, team) VALUES ('ada', 'red')",
        "--root",
        "app.fxdb",
    ]);
    assert!(insert.status.success(), "{insert:?}");
    let identifier = String::from_utf8(insert.stdout).unwrap().trim().to_owned();
    let got = fixture.run(&["get", "users", &identifier, "--root", "app.fxdb", "--json"]);
    assert!(got.status.success(), "{got:?}");
    assert_eq!(json(&got)[&identifier]["name"], "ada");
    // The database is the only thing the commands created.
    let names: Vec<_> = fs::read_dir(&fixture.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["app.fxdb"]);
}

/// A directory root with one collection and one document, returning the
/// document's identifier.
fn seeded_root(fixture: &Fixture, root: &str) -> String {
    let create = fixture.run(&[
        "exec",
        "--request",
        r#"{"op":"createCollection","collection":"users"}"#,
        "--root",
        root,
    ]);
    assert!(create.status.success(), "{create:?}");
    let insert = fixture.run(&[
        "sql",
        "INSERT INTO users (name, team) VALUES ('ada', 'red')",
        "--root",
        root,
    ]);
    assert!(insert.status.success(), "{insert:?}");
    String::from_utf8(insert.stdout).unwrap().trim().to_owned()
}

#[test]
fn pack_and_unpack_carry_a_root_through_a_database() {
    let fixture = Fixture::new("pack");
    let identifier = seeded_root(&fixture, "data");
    let pack = fixture.run(&["pack", "data", "--out", "app.fxdb"]);
    assert!(pack.status.success(), "{pack:?}");
    let report = json(&pack);
    assert_eq!(report["collections"][0]["collection"], "users");
    assert_eq!(report["collections"][0]["documents"], 1);
    let verify = fixture.run(&["verify", "app.fxdb"]);
    assert!(verify.status.success(), "{verify:?}");
    let got = fixture.run(&["get", "users", &identifier, "--root", "app.fxdb", "--json"]);
    assert!(got.status.success(), "{got:?}");
    assert_eq!(json(&got)[&identifier]["name"], "ada");

    let unpack = fixture.run(&["unpack", "app.fxdb", "--out", "restored"]);
    assert!(unpack.status.success(), "{unpack:?}");
    assert_eq!(json(&unpack)["collections"][0]["documents"], 1);
    let got = fixture.run(&["get", "users", &identifier, "--root", "restored", "--json"]);
    assert!(got.status.success(), "{got:?}");
    assert_eq!(json(&got)[&identifier]["team"], "red");
    assert!(fixture.0.join("restored/.collections/users/docs").is_dir());
}

#[test]
fn pack_refuses_sealed_and_an_existing_output() {
    let fixture = Fixture::new("pack-refuse");
    seeded_root(&fixture, "data");
    let sealed = fixture.run(&["pack", "data", "--out", "app.fxdb", "--sealed"]);
    assert!(!sealed.status.success());
    assert!(String::from_utf8_lossy(&sealed.stderr).contains("not supported yet"));
    assert!(!fixture.0.join("app.fxdb").exists());
    fs::write(fixture.0.join("app.fxdb"), b"keep").unwrap();
    let existing = fixture.run(&["pack", "data", "--out", "app.fxdb"]);
    assert!(!existing.status.success());
    assert!(String::from_utf8_lossy(&existing.stderr).contains("already exists"));
    assert_eq!(fs::read(fixture.0.join("app.fxdb")).unwrap(), b"keep");
    let missing = fixture.run(&["pack", "data"]);
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--out"));
}

#[test]
fn a_pack_stopped_after_staging_leaves_no_database() {
    let fixture = Fixture::new("pack-crash");
    let identifier = seeded_root(&fixture, "data");
    let stopped = Command::new(BINARY)
        .current_dir(&fixture.0)
        .args(["pack", "data", "--out", "app.fxdb"])
        .env("FYLO_RUST_FAILPOINT", "after-pack-staging")
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .output()
        .unwrap();
    assert!(!stopped.status.success(), "{stopped:?}");
    // The verified staging file is left, and no database under the name.
    assert!(!fixture.0.join("app.fxdb").exists());
    assert!(fixture.0.join(".app.fxdb.pack-staging").is_file());

    // An injected error, which the process survives, removes its staging
    // file itself.
    let failed = Command::new(BINARY)
        .current_dir(&fixture.0)
        .args(["pack", "data", "--out", "app.fxdb"])
        .env("FYLO_RUST_FAILPOINT", "after-pack-staging")
        .output()
        .unwrap();
    assert!(!failed.status.success(), "{failed:?}");
    assert!(!fixture.0.join("app.fxdb").exists());
    assert!(!fixture.0.join(".app.fxdb.pack-staging").exists());

    let pack = fixture.run(&["pack", "data", "--out", "app.fxdb"]);
    assert!(pack.status.success(), "{pack:?}");
    // Beside the root's own owner lock files, only the database is left.
    let mut names: Vec<_> = fs::read_dir(&fixture.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with(".data.fylo-root-owner"))
        .collect();
    names.sort();
    assert_eq!(names, ["app.fxdb", "data"]);
    let got = fixture.run(&["get", "users", &identifier, "--root", "app.fxdb", "--json"]);
    assert!(got.status.success(), "{got:?}");
}

/// A key generated for one test; no real secret is ever written.
fn generated_secret(name: &str) -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("generated-{name}-{stamp:032x}-{}", std::process::id())
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn create_encrypt_makes_a_database_only_its_key_opens() {
    let fixture = Fixture::new("encrypt");
    let secret = generated_secret("encrypt");
    // --encrypt needs the key.
    let keyless = fixture.run(&["create", "app.fxdb", "--encrypt"]);
    assert!(!keyless.status.success(), "{keyless:?}");
    assert!(String::from_utf8_lossy(&keyless.stderr).contains("EKEY"));
    assert!(!fixture.0.join("app.fxdb").exists());

    let create = fixture.run_keyed(&secret, &["create", "app.fxdb", "--encrypt"]);
    assert!(create.status.success(), "{create:?}");
    assert_eq!(json(&create)["encrypted"], true);
    let again = fixture.run_keyed(&secret, &["create", "app.fxdb"]);
    assert!(!again.status.success(), "{again:?}");
    let collection = fixture.run_keyed(
        &secret,
        &[
            "exec",
            "--request",
            r#"{"op":"createCollection","collection":"users"}"#,
            "--root",
            "app.fxdb",
        ],
    );
    assert!(collection.status.success(), "{collection:?}");
    let insert = fixture.run_keyed(
        &secret,
        &[
            "sql",
            "INSERT INTO users (name, team) VALUES ('NUMBAT-CANARY', 'red')",
            "--root",
            "app.fxdb",
        ],
    );
    assert!(insert.status.success(), "{insert:?}");
    let identifier = String::from_utf8(insert.stdout).unwrap().trim().to_owned();
    let got = fixture.run_keyed(
        &secret,
        &["get", "users", &identifier, "--root", "app.fxdb", "--json"],
    );
    assert!(got.status.success(), "{got:?}");
    assert_eq!(json(&got)[&identifier]["name"], "NUMBAT-CANARY");
    let bytes = fs::read(fixture.0.join("app.fxdb")).unwrap();
    assert!(!contains(&bytes, b"NUMBAT-CANARY"));

    // Without the key, inspect reports it locked, verify passes, and a read
    // is refused.
    let inspect = fixture.run(&["inspect", "app.fxdb"]);
    assert!(inspect.status.success(), "{inspect:?}");
    let report = json(&inspect);
    assert_eq!(report["encrypted"], true);
    assert_eq!(report["locked"], true);
    assert_eq!(report["collections"], serde_json::json!([]));
    let verify = fixture.run(&["verify", "app.fxdb"]);
    assert!(verify.status.success(), "{verify:?}");
    let locked = fixture.run(&["get", "users", &identifier, "--root", "app.fxdb", "--json"]);
    assert!(!locked.status.success(), "{locked:?}");
    assert!(String::from_utf8_lossy(&locked.stderr).contains("EKEY"));
    let wrong = fixture.run_keyed(
        &generated_secret("wrong"),
        &["get", "users", &identifier, "--root", "app.fxdb", "--json"],
    );
    assert!(!wrong.status.success(), "{wrong:?}");
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("does not match"));
    assert_eq!(fs::read(fixture.0.join("app.fxdb")).unwrap(), bytes);

    // With it, inspect lists the collections.
    let inspect = fixture.run_keyed(&secret, &["inspect", "app.fxdb"]);
    assert!(inspect.status.success(), "{inspect:?}");
    assert_eq!(json(&inspect)["locked"], false);
    assert_eq!(json(&inspect)["collections"][0]["name"], "users");
}

#[test]
fn pack_encrypt_seals_a_root_and_unpack_needs_its_key() {
    let fixture = Fixture::new("pack-encrypt");
    let secret = generated_secret("pack");
    let identifier = seeded_root(&fixture, "data");
    let pack = fixture.run_keyed(&secret, &["pack", "data", "--out", "app.fxdb", "--encrypt"]);
    assert!(pack.status.success(), "{pack:?}");
    let bytes = fs::read(fixture.0.join("app.fxdb")).unwrap();
    assert!(!contains(&bytes, b"\"ada\""));
    let report = json(&fixture.run(&["inspect", "app.fxdb"]));
    assert_eq!(report["encrypted"], true);
    let got = fixture.run_keyed(
        &secret,
        &["get", "users", &identifier, "--root", "app.fxdb", "--json"],
    );
    assert!(got.status.success(), "{got:?}");
    assert_eq!(json(&got)[&identifier]["name"], "ada");

    let locked = fixture.run(&["unpack", "app.fxdb", "--out", "restored"]);
    assert!(!locked.status.success(), "{locked:?}");
    assert!(String::from_utf8_lossy(&locked.stderr).contains("EKEY"));
    let unpack = fixture.run_keyed(&secret, &["unpack", "app.fxdb", "--out", "restored"]);
    assert!(unpack.status.success(), "{unpack:?}");
    assert_eq!(json(&unpack)["collections"][0]["documents"], 1);
}

#[test]
fn create_makes_a_plain_database_by_default() {
    let fixture = Fixture::new("create-plain");
    let create = fixture.run(&["create", "app.fxdb"]);
    assert!(create.status.success(), "{create:?}");
    assert_eq!(json(&create)["encrypted"], false);
    assert_eq!(
        json(&fixture.run(&["inspect", "app.fxdb"]))["encrypted"],
        false
    );
    let directory = fixture.run(&["create", "data"]);
    assert!(!directory.status.success(), "{directory:?}");
}

#[test]
fn compact_shrinks_and_salvage_writes_out_a_database() {
    let fixture = Fixture::new("compact");
    let secret = generated_secret("compact");
    let create = fixture.run_keyed(&secret, &["create", "app.fxdb", "--encrypt"]);
    assert!(create.status.success(), "{create:?}");
    let collection = fixture.run_keyed(
        &secret,
        &[
            "exec",
            "--request",
            r#"{"op":"createCollection","collection":"users"}"#,
            "--root",
            "app.fxdb",
        ],
    );
    assert!(collection.status.success(), "{collection:?}");
    let mut identifiers = Vec::new();
    for team in ["red", "blue", "gone"] {
        let insert = fixture.run_keyed(
            &secret,
            &[
                "sql",
                &format!("INSERT INTO users (name, team) VALUES ('ada', '{team}')"),
                "--root",
                "app.fxdb",
            ],
        );
        assert!(insert.status.success(), "{insert:?}");
        identifiers.push(String::from_utf8(insert.stdout).unwrap().trim().to_owned());
    }
    let delete = fixture.run_keyed(
        &secret,
        &[
            "sql",
            "DELETE FROM users WHERE team = 'gone'",
            "--root",
            "app.fxdb",
        ],
    );
    assert!(delete.status.success(), "{delete:?}");

    // Without the key the database is locked, and nothing changes.
    let bytes = fs::read(fixture.0.join("app.fxdb")).unwrap();
    for arguments in [
        &["compact", "app.fxdb", "--shrink"][..],
        &["salvage", "app.fxdb", "--out", "salvaged"][..],
    ] {
        let locked = fixture.run(arguments);
        assert!(!locked.status.success(), "{locked:?}");
        assert!(String::from_utf8_lossy(&locked.stderr).contains("EKEY"));
    }
    assert_eq!(fs::read(fixture.0.join("app.fxdb")).unwrap(), bytes);
    assert!(!fixture.0.join("salvaged").exists());

    let compact = fixture.run_keyed(&secret, &["compact", "app.fxdb", "--shrink"]);
    assert!(compact.status.success(), "{compact:?}");
    let report = json(&compact);
    assert_eq!(report["shrink"], true);
    assert_eq!(report["collections"][0]["collection"], "users");
    assert!(report["collections"][0]["segmentsCompacted"].as_u64() >= Some(1));
    // A database this small gains the compacted copies' extents; a shrink
    // gives back only what lies free.
    assert!(
        report["fileBytesAfter"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    let verify = fixture.run(&["verify", "app.fxdb"]);
    assert!(verify.status.success(), "{verify:?}");
    let got = fixture.run_keyed(
        &secret,
        &[
            "get",
            "users",
            &identifiers[1],
            "--root",
            "app.fxdb",
            "--json",
        ],
    );
    assert!(got.status.success(), "{got:?}");
    assert_eq!(json(&got)[&identifiers[1]]["team"], "blue");

    let salvage = fixture.run_keyed(&secret, &["salvage", "app.fxdb", "--out", "salvaged"]);
    assert!(salvage.status.success(), "{salvage:?}");
    let report = json(&salvage);
    assert_eq!(report["collections"][0]["collection"], "users");
    assert_eq!(report["collections"][0]["damaged"], serde_json::json!([]));
    let got = fixture.run(&[
        "get",
        "users",
        &identifiers[0],
        "--root",
        "salvaged",
        "--json",
    ]);
    assert!(got.status.success(), "{got:?}");
    assert_eq!(json(&got)[&identifiers[0]]["team"], "red");
    assert!(fixture.0.join("salvaged/.collections/users/docs").is_dir());
}
