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
