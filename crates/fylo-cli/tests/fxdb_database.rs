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
