//! Hardware identities through the CLI (ADR 0021, phase 8): `enclave:` keys
//! in this Mac's Secure Enclave. Each test keeps the keys' token blobs in
//! its own throwaway keychain (`FYLO_KEYCHAIN`), deleted afterwards.
#![cfg(target_os = "macos")]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_fylo-rust");
const SECURITY: &str = "/usr/bin/security";
const CANARY: &str = "NUMBAT-CANARY";

struct Fixture {
    directory: PathBuf,
    keychain: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("fylo-cli-hardware-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let keychain = directory.join("test.keychain-db");
        let password = format!("fylo-test-{}", std::process::id());
        security(&[
            "create-keychain",
            "-p",
            &password,
            keychain.to_str().unwrap(),
        ]);
        security(&["set-keychain-settings", keychain.to_str().unwrap()]);
        security(&[
            "unlock-keychain",
            "-p",
            &password,
            keychain.to_str().unwrap(),
        ]);
        Self {
            directory,
            keychain,
        }
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(BINARY)
            .current_dir(&self.directory)
            .args(arguments)
            .env("FYLO_KEYCHAIN", &self.keychain)
            .env_remove("FYLO_IDENTITY")
            .env_remove("FYLO_TRUST_ANCHOR")
            .output()
            .unwrap()
    }

    fn reference(name: &str) -> String {
        format!("enclave:com.delma.fylo-test.{name}.{}", std::process::id())
    }

    /// Create `app.vdbx` with an enclave identity as its admin; returns the
    /// admin's reference and identifier.
    fn protected(&self, name: &str) -> (String, String) {
        let reference = Self::reference(name);
        let admin = stdout(&self.run(&["identity", "create", "--store", &reference]));
        let created = json(&self.run(&["create", "app.vdbx", "--admin", &reference]));
        assert_eq!(created["protected"], true);
        assert_eq!(created["trustAnchor"], admin.as_str());
        (reference, admin)
    }

    fn as_identity(&self, reference: &str, anchor: &str, arguments: &[&str]) -> Output {
        let mut all = arguments.to_vec();
        all.extend(["--identity", reference, "--trust-anchor", anchor]);
        self.run(&all)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = Command::new(SECURITY)
            .args(["delete-keychain", self.keychain.to_str().unwrap()])
            .output();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn security(arguments: &[&str]) {
    let output = Command::new(SECURITY).args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "security {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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

fn failure(output: &Output) -> String {
    assert!(!output.status.success(), "fylo unexpectedly succeeded");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> Value {
    serde_json::from_str(&stdout(output)).unwrap()
}

fn write_user(fixture: &Fixture, reference: &str, anchor: &str) -> String {
    stdout(&fixture.as_identity(
        reference,
        anchor,
        &[
            "exec",
            "--request",
            r#"{"op":"createCollection","collection":"users"}"#,
            "--root",
            "app.vdbx",
        ],
    ));
    stdout(&fixture.as_identity(
        reference,
        anchor,
        &[
            "sql",
            &format!("INSERT INTO users (name) VALUES ('{CANARY}')"),
            "--root",
            "app.vdbx",
        ],
    ))
}

#[test]
fn create_makes_an_enclave_key_and_show_reads_it() {
    let fixture = Fixture::new("show");
    let reference = Fixture::reference("show");
    let created = json(&fixture.run(&["identity", "create", "--store", &reference, "--json"]));
    assert!(created["id"].as_str().unwrap().starts_with("fid1:p256:"));
    assert_eq!(created["reference"], reference.as_str());
    let shown = json(&fixture.run(&["identity", "show", &reference, "--json"]));
    assert_eq!(shown, created);
}

#[test]
fn create_never_replaces_an_enclave_key() {
    let fixture = Fixture::new("exists");
    let reference = Fixture::reference("exists");
    let created = stdout(&fixture.run(&["identity", "create", "--store", &reference]));
    let error = failure(&fixture.run(&["identity", "create", "--store", &reference]));
    assert!(error.contains("EIDENTITY_EXISTS"), "{error}");
    assert_eq!(
        stdout(&fixture.run(&["identity", "show", &reference])),
        created
    );
}

#[test]
fn show_reports_a_missing_enclave_key() {
    let fixture = Fixture::new("missing");
    let error = failure(&fixture.run(&["identity", "show", &Fixture::reference("missing")]));
    assert!(error.contains("EIDENTITY_NOT_FOUND"), "{error}");
}

#[test]
fn an_enclave_admin_writes_and_reads_a_protected_database() {
    let fixture = Fixture::new("round-trip");
    let (reference, admin) = fixture.protected("round-trip");
    let identifier = write_user(&fixture, &reference, &admin);
    let got = json(&fixture.as_identity(
        &reference,
        &admin,
        &["get", "users", &identifier, "--root", "app.vdbx", "--json"],
    ));
    assert_eq!(got[&identifier]["name"], CANARY);
    let bytes = fs::read(fixture.directory.join("app.vdbx")).unwrap();
    assert!(
        !bytes
            .windows(CANARY.len())
            .any(|window| window == CANARY.as_bytes())
    );
}

#[test]
fn verify_as_the_enclave_admin_checks_every_signature() {
    let fixture = Fixture::new("verify");
    let (reference, admin) = fixture.protected("verify");
    write_user(&fixture, &reference, &admin);
    let report = json(&fixture.as_identity(&reference, &admin, &["verify", "app.vdbx"]));
    assert_eq!(report["ok"], true, "{report}");
}

#[test]
fn a_software_reader_reads_what_the_enclave_admin_wrote() {
    let fixture = Fixture::new("reader");
    let (reference, admin) = fixture.protected("reader");
    let identifier = write_user(&fixture, &reference, &admin);
    stdout(&fixture.run(&["identity", "create", "--store", "file:reader.pem"]));
    let reader = json(&fixture.run(&["identity", "show", "file:reader.pem", "--json"]));
    let public_key = reader["publicKey"].as_str().unwrap();
    stdout(&fixture.as_identity(
        &reference,
        &admin,
        &[
            "access",
            "grant",
            "app.vdbx",
            "--grantee",
            "reader",
            "--key",
            public_key,
            "--actions",
            "read",
            "--collection",
            "users",
        ],
    ));
    let got = json(&fixture.as_identity(
        "file:reader.pem",
        &admin,
        &["get", "users", &identifier, "--root", "app.vdbx", "--json"],
    ));
    assert_eq!(got[&identifier]["name"], CANARY);
}
