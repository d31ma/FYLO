#![cfg(unix)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_fylo-rust");
const ACCESS_XATTR: &str = "user.fylo.access";

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("fylo-cli-migration-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.run_with(arguments, &[])
    }

    fn run_with(&self, arguments: &[&str], environment: &[(&str, &str)]) -> Output {
        Command::new(BINARY)
            .current_dir(&self.0)
            .args(arguments)
            .env_remove("FYLO_IDENTITY")
            .env_remove("FYLO_TRUST_ANCHOR")
            .envs(environment.iter().copied())
            .output()
            .unwrap()
    }

    /// Run with `keyed.json`, which names the shared database key
    /// `secret`.
    fn run_keyed(&self, secret: &str, arguments: &[&str]) -> Output {
        fs::write(
            self.0.join("keyed.json"),
            r#"{"fylo":{"encryption":{"key":"${FYLO_CLI_TEST_DATABASE_KEY}","file":true}}}"#,
        )
        .unwrap();
        let mut all = arguments.to_vec();
        all.extend(["--config", "keyed.json"]);
        self.run_with(&all, &[("FYLO_CLI_TEST_DATABASE_KEY", secret)])
    }

    /// A directory root `root` with the collection `users` and the
    /// documents `names`, returning their identifiers in order.
    fn seeded(&self, root: &str, names: &[&str]) -> Vec<String> {
        let create = self.run(&[
            "exec",
            "--request",
            r#"{"op":"createCollection","collection":"users"}"#,
            "--root",
            root,
        ]);
        assert!(create.status.success(), "{create:?}");
        names
            .iter()
            .map(|name| {
                let sql = format!("INSERT INTO users (name) VALUES ('{name}')");
                let insert = self.run(&["sql", &sql, "--root", root]);
                assert!(insert.status.success(), "{insert:?}");
                String::from_utf8(insert.stdout).unwrap().trim().to_owned()
            })
            .collect()
    }

    /// Give the record file of `identifier` the POSTIX access descriptor
    /// an earlier release wrote: this process's own user and group, and
    /// `mode`.
    fn describe(&self, root: &str, identifier: &str, mode: u32) -> (u32, u32) {
        let file = record_file(&self.0.join(root).join(".collections/users"), identifier);
        let metadata = fs::metadata(&file).unwrap();
        let (uid, gid) = (metadata.uid(), metadata.gid());
        let descriptor = format!(r#"{{"version":1,"uid":{uid},"gid":{gid},"mode":{mode}}}"#);
        xattr::set(&file, ACCESS_XATTR, descriptor.as_bytes()).unwrap();
        (uid, gid)
    }

    fn identity(&self, name: &str) -> String {
        let reference = format!("file:{name}.pem");
        let created = self.run(&["identity", "create", "--store", &reference]);
        assert!(created.status.success(), "{created:?}");
        String::from_utf8(created.stdout).unwrap().trim().to_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The file the file layout keeps `identifier`'s live document in.
fn record_file(collection: &Path, identifier: &str) -> PathBuf {
    let name = format!("{identifier}.json");
    let mut pending = vec![collection.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|file| file == name.as_str()) {
                return path;
            }
        }
    }
    panic!(
        "no record file for {identifier} under {}",
        collection.display()
    );
}

/// Every path under `directory` with its length and modification time.
fn tree(directory: &Path) -> Vec<(PathBuf, u64, i64, i64)> {
    let mut entries = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                pending.push(path.clone());
            }
            entries.push((
                path,
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            ));
        }
    }
    entries.sort();
    entries
}

fn json(output: &Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn report_lists_the_access_descriptors_of_a_directory_root() {
    let fixture = Fixture::new("report-directory");
    let ids = fixture.seeded("data", &["ada", "grace", "edsger"]);
    let (uid, gid) = fixture.describe("data", &ids[0], 0o640);
    fixture.describe("data", &ids[1], 0o640);
    fixture.describe("data", &ids[2], 0o600);

    let report = json(&fixture.run(&["access", "report", "data"]));
    assert_eq!(report["kind"], "directory");
    assert_eq!(report["encryption"], "none");
    assert_eq!(report["locked"], false);
    assert_eq!(report["recordsWithAccess"], 3);
    assert_eq!(report["blockers"], serde_json::json!([]));
    let users = &report["collections"][0];
    assert_eq!(users["collection"], "users");
    assert_eq!(users["layout"], "files");
    assert_eq!(users["documents"], 3);
    assert_eq!(users["deleted"], 0);
    assert_eq!(
        users["access"],
        serde_json::json!([
            {"uid": uid, "gid": gid, "mode": "0600", "records": 1},
            {"uid": uid, "gid": gid, "mode": "0640", "records": 2},
        ])
    );
    let command = report["command"].as_str().unwrap();
    assert!(command.contains("--protect --admin"), "{command}");
    assert!(command.contains("--drop-record-access"), "{command}");
    let changes = report["changes"].to_string();
    assert!(
        changes.contains("3 records carry POSTIX access descriptors"),
        "{changes}"
    );
    assert!(changes.contains("plain text"), "{changes}");
}

#[test]
fn report_writes_nothing() {
    let fixture = Fixture::new("report-read-only");
    let ids = fixture.seeded("data", &["ada"]);
    fixture.describe("data", &ids[0], 0o640);
    let before = tree(&fixture.0.join("data"));
    json(&fixture.run(&["access", "report", "data"]));
    assert_eq!(tree(&fixture.0.join("data")), before);
}

#[test]
fn report_without_descriptors_needs_no_record_access_flag() {
    let fixture = Fixture::new("report-plain");
    fixture.seeded("data", &["ada"]);
    let report = json(&fixture.run(&["access", "report", "data"]));
    assert_eq!(report["recordsWithAccess"], 0);
    assert_eq!(report["collections"][0]["access"], serde_json::json!([]));
    let command = report["command"].as_str().unwrap();
    assert!(!command.contains("record-access"), "{command}");
}

#[test]
fn report_names_what_a_database_cannot_hold() {
    let fixture = Fixture::new("report-blockers");
    fixture.seeded("data", &["ada"]);
    fs::create_dir_all(fixture.0.join("data/.buckets/photos")).unwrap();
    let report = json(&fixture.run(&["access", "report", "data"]));
    let blockers = report["blockers"].to_string();
    assert!(blockers.contains("bucket \\\"photos\\\""), "{blockers}");
}

#[test]
fn report_on_a_shared_key_database_needs_its_key() {
    let fixture = Fixture::new("report-shared-key");
    let ids = fixture.seeded("data", &["ada", "grace"]);
    fixture.describe("data", &ids[0], 0o640);
    let pack = fixture.run_keyed(
        "report-secret-0123456789abcdef-0123456789",
        &["pack", "data", "--out", "app.vdbx", "--encrypt"],
    );
    assert!(pack.status.success(), "{pack:?}");

    let locked = json(&fixture.run(&["access", "report", "app.vdbx"]));
    assert_eq!(locked["kind"], "database");
    assert_eq!(locked["encryption"], "shared-key");
    assert_eq!(locked["locked"], true);
    assert_eq!(locked["collections"], serde_json::json!([]));

    let open = json(&fixture.run_keyed(
        "report-secret-0123456789abcdef-0123456789",
        &["access", "report", "app.vdbx"],
    ));
    assert_eq!(open["locked"], false);
    assert_eq!(open["recordsWithAccess"], 1);
    assert_eq!(open["collections"][0]["layout"], "segments");
    assert_eq!(open["collections"][0]["documents"], 2);
    assert!(open["changes"].to_string().contains("fylo.encryption.key"));
}

#[test]
fn report_on_a_protected_database_has_nothing_to_migrate() {
    let fixture = Fixture::new("report-protected");
    let admin = fixture.identity("admin");
    let created = fixture.run(&["create", "app.vdbx", "--admin", "file:admin.pem"]);
    assert!(created.status.success(), "{created:?}");

    let locked = json(&fixture.run(&["access", "report", "app.vdbx"]));
    assert_eq!(locked["encryption"], "protected");
    assert_eq!(locked["locked"], true);
    assert_eq!(locked["command"], Value::Null);

    let open = json(&fixture.run(&[
        "access",
        "report",
        "app.vdbx",
        "--identity",
        "file:admin.pem",
        "--trust-anchor",
        &admin,
    ]));
    assert_eq!(open["locked"], false);
    assert!(open["changes"].to_string().contains("nothing to migrate"));
}

#[test]
fn report_needs_a_root() {
    let fixture = Fixture::new("report-usage");
    let output = fixture.run(&["access", "report"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("fylo access report <root|db.vdbx>"));
}
