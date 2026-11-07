use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_fylo-rust");
const CANARY: &str = "BILBY-CANARY";

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("fylo-cli-protected-{name}-{}", std::process::id()));
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

    /// Make an identity in the key file `<name>.pem` and return its
    /// identifier.
    fn identity(&self, name: &str) -> String {
        let reference = format!("file:{name}.pem");
        stdout(&self.run(&["identity", "create", "--store", &reference]))
    }

    /// Create `app.vdbx` with the identity `admin` as its admin, and return
    /// the admin's identifier.
    fn protected(&self) -> String {
        let admin = self.identity("admin");
        let created = json(&self.run(&["create", "app.vdbx", "--admin", "file:admin.pem"]));
        assert_eq!(created["protected"], true);
        assert_eq!(created["trustAnchor"], admin.as_str());
        admin
    }

    fn as_admin<'a>(&self, anchor: &'a str, arguments: &[&'a str]) -> Output {
        let mut all = arguments.to_vec();
        all.extend(["--identity", "file:admin.pem", "--trust-anchor", anchor]);
        self.run(&all)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "fylo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .trim()
        .to_owned()
}

fn json(output: &Output) -> Value {
    serde_json::from_str(&stdout(output)).unwrap()
}

fn failure(output: &Output) -> String {
    assert!(!output.status.success(), "fylo unexpectedly succeeded");
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn contains(bytes: &[u8], text: &str) -> bool {
    bytes
        .windows(text.len())
        .any(|window| window == text.as_bytes())
}

fn write_user(fixture: &Fixture, anchor: &str) -> String {
    stdout(&fixture.as_admin(
        anchor,
        &[
            "exec",
            "--request",
            r#"{"op":"createCollection","collection":"users"}"#,
            "--root",
            "app.vdbx",
        ],
    ));
    let insert = fixture.as_admin(
        anchor,
        &[
            "sql",
            &format!("INSERT INTO users (name) VALUES ('{CANARY}')"),
            "--root",
            "app.vdbx",
        ],
    );
    stdout(&insert)
}

#[test]
fn create_admin_makes_a_protected_database_with_its_creator_as_anchor() {
    let fixture = Fixture::new("create");
    let admin = fixture.protected();
    let listed = json(&fixture.as_admin(&admin, &["access", "list", "app.vdbx"]));
    assert_eq!(listed["identity"], admin.as_str());
    assert_eq!(listed["trustAnchor"], admin.as_str());
    assert_eq!(listed["admin"], true);
    assert_eq!(listed["trustedOnFirstUse"], false);
    assert_eq!(listed["policy"]["version"], 1);
    assert_eq!(listed["policy"]["rules"][0]["actions"][0], "admin");
}

#[test]
fn the_admin_writes_and_reads_and_the_file_hides_the_documents() {
    let fixture = Fixture::new("round-trip");
    let admin = fixture.protected();
    let identifier = write_user(&fixture, &admin);
    let got = json(&fixture.as_admin(
        &admin,
        &["get", "users", &identifier, "--root", "app.vdbx", "--json"],
    ));
    assert_eq!(got[&identifier]["name"], CANARY);
    let bytes = fs::read(fixture.0.join("app.vdbx")).unwrap();
    assert!(!contains(&bytes, CANARY));
    assert!(!contains(&bytes, "users"));
}

#[test]
fn without_an_identity_reads_are_refused_and_inspect_reports_it_locked() {
    let fixture = Fixture::new("locked");
    let admin = fixture.protected();
    let identifier = write_user(&fixture, &admin);
    let error = failure(&fixture.run(&["get", "users", &identifier, "--root", "app.vdbx"]));
    assert!(error.contains("ENOIDENTITY"), "{error}");
    let report = json(&fixture.run(&["inspect", "app.vdbx"]));
    assert_eq!(report["protected"], true);
    assert_eq!(report["locked"], true);
    assert_eq!(report["collections"], serde_json::json!([]));
}

#[test]
fn inspect_with_the_identity_lists_the_collections() {
    let fixture = Fixture::new("inspect");
    let admin = fixture.protected();
    write_user(&fixture, &admin);
    let report = json(&fixture.as_admin(&admin, &["inspect", "app.vdbx"]));
    assert_eq!(report["locked"], false);
    assert_eq!(report["collections"][0]["name"], "users");
}

#[test]
fn verify_checks_a_protected_database_without_an_identity() {
    let fixture = Fixture::new("verify");
    let admin = fixture.protected();
    write_user(&fixture, &admin);
    let report = json(&fixture.run(&["verify", "app.vdbx"]));
    assert_eq!(report["ok"], true, "{report}");
}

#[test]
fn an_identity_without_a_grant_is_refused() {
    let fixture = Fixture::new("stranger");
    let admin = fixture.protected();
    fixture.identity("stranger");
    let error = failure(&fixture.run(&[
        "access",
        "list",
        "app.vdbx",
        "--identity",
        "file:stranger.pem",
        "--trust-anchor",
        &admin,
    ]));
    assert!(error.contains("no grant"), "{error}");
}

#[test]
fn an_unpinned_open_is_refused_with_the_anchor_to_pin() {
    let fixture = Fixture::new("unpinned");
    let admin = fixture.protected();
    let error =
        failure(&fixture.run(&["access", "list", "app.vdbx", "--identity", "file:admin.pem"]));
    assert!(
        error.contains("EPOLICY") || error.contains(&admin),
        "{error}"
    );
    assert!(error.contains(&admin), "{error}");
}

#[test]
fn another_pinned_anchor_is_refused() {
    let fixture = Fixture::new("other-anchor");
    fixture.protected();
    let other = fixture.identity("other");
    let error = failure(&fixture.as_admin(&other, &["access", "list", "app.vdbx"]));
    assert!(error.contains("not by the pinned trust anchor"), "{error}");
}

#[test]
fn trust_on_first_use_opens_and_warns() {
    let fixture = Fixture::new("first-use");
    fixture.protected();
    let output = fixture.run(&[
        "access",
        "list",
        "app.vdbx",
        "--identity",
        "file:admin.pem",
        "--trust-on-first-use",
    ]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("warning: --trust-on-first-use"));
    assert_eq!(json(&output)["trustedOnFirstUse"], true);
}

#[test]
fn the_environment_names_the_identity_and_the_anchor() {
    let fixture = Fixture::new("environment");
    let admin = fixture.protected();
    let listed = json(&fixture.run_with(
        &["access", "list", "app.vdbx"],
        &[
            ("FYLO_IDENTITY", "file:admin.pem"),
            ("FYLO_TRUST_ANCHOR", &admin),
        ],
    ));
    assert_eq!(listed["identity"], admin.as_str());
}

#[test]
fn the_configuration_names_the_identity_and_the_anchor() {
    let fixture = Fixture::new("configuration");
    let admin = fixture.protected();
    let configuration = serde_json::json!({
        "fylo": { "identity": "file:admin.pem", "trustAnchor": admin }
    });
    fs::write(
        fixture.0.join("application.json"),
        configuration.to_string(),
    )
    .unwrap();
    let listed = json(&fixture.run(&["access", "list", "app.vdbx"]));
    assert_eq!(listed["identity"], admin.as_str());
}

#[test]
fn access_list_refuses_a_database_that_is_not_protected() {
    let fixture = Fixture::new("unprotected");
    stdout(&fixture.run(&["create", "app.vdbx", "--unprotected"]));
    let error = failure(&fixture.run(&["access", "list", "app.vdbx"]));
    assert!(error.contains("is not a protected"), "{error}");
}

#[test]
fn create_admin_takes_no_unprotected() {
    let fixture = Fixture::new("admin-unprotected");
    fixture.identity("admin");
    let error = failure(&fixture.run(&[
        "create",
        "app.vdbx",
        "--admin",
        "file:admin.pem",
        "--unprotected",
    ]));
    assert!(error.contains("takes neither"), "{error}");
}
