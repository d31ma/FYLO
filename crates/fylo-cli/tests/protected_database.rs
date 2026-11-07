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

fn public_key(fixture: &Fixture, name: &str) -> String {
    let shown = json(&fixture.run(&["identity", "show", &format!("file:{name}.pem"), "--json"]));
    shown["publicKey"].as_str().unwrap().to_owned()
}

fn as_reader<'a>(admin: &'a str, arguments: &[&'a str]) -> Vec<&'a str> {
    let mut all = arguments.to_vec();
    all.extend(["--identity", "file:reader.pem", "--trust-anchor", admin]);
    all
}

#[test]
fn grant_lets_a_reader_read_one_collection_and_revoke_takes_it_back() {
    let fixture = Fixture::new("grant-revoke");
    let admin = fixture.protected();
    let identifier = write_user(&fixture, &admin);
    let reader = fixture.identity("reader");
    let key = public_key(&fixture, "reader");
    let granted = json(&fixture.as_admin(
        &admin,
        &[
            "access",
            "grant",
            "app.vdbx",
            "--grantee",
            "reader",
            "--key",
            &key,
            "--actions",
            "read",
            "--collection",
            "users",
        ],
    ));
    assert_eq!(granted["policy"]["version"], 2);
    assert_eq!(
        granted["policy"]["rules"][1]["id"],
        "reader-collection:users"
    );

    let listed = json(&fixture.run(&as_reader(&admin, &["access", "list", "app.vdbx"])));
    assert_eq!(listed["identity"], reader.as_str());
    assert_eq!(listed["admin"], false);
    assert_eq!(listed["grants"]["users"], "read");
    let got = json(&fixture.run(&as_reader(
        &admin,
        &["get", "users", &identifier, "--root", "app.vdbx", "--json"],
    )));
    assert_eq!(got[&identifier]["name"], CANARY);
    let insert = failure(&fixture.run(&as_reader(
        &admin,
        &[
            "sql",
            "INSERT INTO users (name) VALUES ('x')",
            "--root",
            "app.vdbx",
        ],
    )));
    assert!(insert.contains("EACCES"), "{insert}");

    let revoked = json(&fixture.as_admin(
        &admin,
        &[
            "access",
            "revoke",
            "app.vdbx",
            "--rule",
            "reader-collection:users",
        ],
    ));
    assert_eq!(revoked["policy"]["version"], 3);
    let refused = failure(&fixture.run(&as_reader(&admin, &["access", "list", "app.vdbx"])));
    assert!(refused.contains("no grant"), "{refused}");
}

#[test]
fn a_grant_by_an_identity_that_is_not_an_admin_is_refused() {
    let fixture = Fixture::new("grant-not-admin");
    let admin = fixture.protected();
    fixture.identity("reader");
    let key = public_key(&fixture, "reader");
    stdout(&fixture.as_admin(
        &admin,
        &[
            "access",
            "grant",
            "app.vdbx",
            "--grantee",
            "reader",
            "--key",
            &key,
            "--actions",
            "read",
            "--all-collections",
        ],
    ));
    let refused = failure(&fixture.run(&as_reader(
        &admin,
        &[
            "access",
            "grant",
            "app.vdbx",
            "--grantee",
            "reader",
            "--key",
            &key,
            "--actions",
            "admin",
            "--database",
            "--rule",
            "escalate",
        ],
    )));
    assert!(refused.contains("is not an admin"), "{refused}");
}

#[test]
fn grant_needs_exactly_one_resource() {
    let fixture = Fixture::new("grant-resource");
    let admin = fixture.protected();
    fixture.identity("reader");
    let key = public_key(&fixture, "reader");
    let refused = failure(&fixture.as_admin(
        &admin,
        &[
            "access",
            "grant",
            "app.vdbx",
            "--grantee",
            "reader",
            "--key",
            &key,
            "--actions",
            "read",
            "--database",
            "--all-collections",
        ],
    ));
    assert!(refused.contains("exactly one"), "{refused}");
}

#[test]
fn verify_with_the_identity_checks_every_signature() {
    let fixture = Fixture::new("verify-signed");
    let admin = fixture.protected();
    write_user(&fixture, &admin);
    let report = json(&fixture.as_admin(&admin, &["verify", "app.vdbx"]));
    assert_eq!(report["ok"], true, "{report}");
}

fn grant_reader(fixture: &Fixture, admin: &str) {
    let key = public_key(fixture, "reader");
    json(&fixture.as_admin(
        admin,
        &[
            "access",
            "grant",
            "app.vdbx",
            "--grantee",
            "reader",
            "--key",
            &key,
            "--actions",
            "read",
            "--collection",
            "users",
            "--rule",
            "readers",
        ],
    ));
}

#[test]
fn rekey_with_the_admin_identity_rewrites_the_database_under_fresh_keys() {
    let fixture = Fixture::new("rekey-protected");
    let admin = fixture.protected();
    let identifier = write_user(&fixture, &admin);
    fixture.identity("reader");
    grant_reader(&fixture, &admin);
    let before = fs::read(fixture.0.join("app.vdbx")).unwrap();
    let rekeyed = json(&fixture.as_admin(&admin, &["rekey", "app.vdbx"]));
    assert_eq!(rekeyed["protected"], true);
    assert_eq!(rekeyed["rekeyed"], "database");
    assert_eq!(rekeyed["collections"][0]["documents"], 1);
    let listed = json(&fixture.as_admin(&admin, &["access", "list", "app.vdbx"]));
    assert_eq!(listed["policy"]["version"], 3);
    assert_eq!(listed["policy"]["change"]["kind"], "rekey");
    assert_eq!(listed["trustAnchor"], admin.as_str());
    let got = json(&fixture.run(&as_reader(
        &admin,
        &["get", "users", &identifier, "--root", "app.vdbx", "--json"],
    )));
    assert_eq!(got[&identifier]["name"], CANARY);
    let report = json(&fixture.as_admin(&admin, &["verify", "app.vdbx"]));
    assert_eq!(report["ok"], true, "{report}");
    let after = fs::read(fixture.0.join("app.vdbx")).unwrap();
    assert!(!contains(&after, CANARY));
    // The catalog key is new, so the superblock's key-check value is too.
    assert_ne!(before[..4096], after[..4096]);
}

#[test]
fn a_revoked_reader_that_kept_its_old_keys_reads_nothing_after_a_collection_rekey() {
    let fixture = Fixture::new("rekey-revoked");
    let admin = fixture.protected();
    let identifier = write_user(&fixture, &admin);
    fixture.identity("reader");
    grant_reader(&fixture, &admin);
    let kept = fs::read(fixture.0.join("app.vdbx")).unwrap();
    json(&fixture.as_admin(
        &admin,
        &["access", "revoke", "app.vdbx", "--rule", "readers"],
    ));
    let rekeyed = json(&fixture.as_admin(&admin, &["rekey", "app.vdbx", "--collection", "users"]));
    assert_eq!(rekeyed["rekeyed"], "collection");
    // The reader puts back the access slots it kept, with its wraps of the
    // old catalog key and the old epoch: the revocation rotated the catalog
    // key, and the rekey sealed the collection under a new epoch.
    let mut spliced = fs::read(fixture.0.join("app.vdbx")).unwrap();
    let magic = b"VDXACCS\0";
    let find = |bytes: &[u8]| {
        bytes
            .windows(magic.len())
            .position(|window| window == magic)
            .unwrap()
    };
    let (old, new) = (find(&kept), find(&spliced));
    assert_eq!(old, new);
    let slots = 512 * 1024;
    spliced[new..new + slots].copy_from_slice(&kept[old..old + slots]);
    fs::write(fixture.0.join("app.vdbx"), &spliced).unwrap();
    let listed = failure(&fixture.run(&as_reader(&admin, &["access", "list", "app.vdbx"])));
    assert!(listed.contains("EPOLICY"), "{listed}");
    let read = fixture.run(&as_reader(
        &admin,
        &["get", "users", &identifier, "--root", "app.vdbx", "--json"],
    ));
    assert!(!read.status.success());
    assert!(!contains(&read.stdout, CANARY));
}

#[test]
fn a_rekey_that_stops_before_the_rename_leaves_the_database_as_it_was() {
    let fixture = Fixture::new("rekey-stopped");
    let admin = fixture.protected();
    let identifier = write_user(&fixture, &admin);
    let stopped = Command::new(BINARY)
        .current_dir(&fixture.0)
        .args([
            "rekey",
            "app.vdbx",
            "--identity",
            "file:admin.pem",
            "--trust-anchor",
            &admin,
        ])
        .env_remove("FYLO_IDENTITY")
        .env_remove("FYLO_TRUST_ANCHOR")
        .env("FYLO_RUST_FAILPOINT", "after-pack-staging")
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .output()
        .unwrap();
    assert!(!stopped.status.success(), "{stopped:?}");
    let listed = json(&fixture.as_admin(&admin, &["access", "list", "app.vdbx"]));
    assert_eq!(listed["policy"]["version"], 1);
    let got = json(&fixture.as_admin(
        &admin,
        &["get", "users", &identifier, "--root", "app.vdbx", "--json"],
    ));
    assert_eq!(got[&identifier]["name"], CANARY);

    json(&fixture.as_admin(&admin, &["rekey", "app.vdbx"]));
    let staging: Vec<String> = fs::read_dir(&fixture.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("staging"))
        .collect();
    assert!(staging.is_empty(), "{staging:?}");
    let got = json(&fixture.as_admin(
        &admin,
        &["get", "users", &identifier, "--root", "app.vdbx", "--json"],
    ));
    assert_eq!(got[&identifier]["name"], CANARY);
}

#[test]
fn rekey_of_a_protected_database_takes_no_shared_key() {
    let fixture = Fixture::new("rekey-shared");
    let admin = fixture.protected();
    let refused = failure(&fixture.run_with(
        &[
            "rekey",
            "app.vdbx",
            "--new-key",
            "${NEW_KEY}",
            "--identity",
            "file:admin.pem",
            "--trust-anchor",
            &admin,
        ],
        &[("NEW_KEY", "a-new-key-of-at-least-thirty-two-bytes")],
    ));
    assert!(refused.contains("--new-key"), "{refused}");
}

fn write_secret(fixture: &Fixture, admin: &str) {
    stdout(&fixture.as_admin(
        admin,
        &[
            "exec",
            "--request",
            r#"{"op":"createCollection","collection":"secrets"}"#,
            "--root",
            "app.vdbx",
        ],
    ));
    stdout(&fixture.as_admin(
        admin,
        &[
            "sql",
            "INSERT INTO secrets (code) VALUES ('WOMBAT-SECRET')",
            "--root",
            "app.vdbx",
        ],
    ));
}

#[test]
fn a_sealed_pack_of_a_protected_database_keeps_each_grant() {
    let fixture = Fixture::new("sealed-protected");
    let admin = fixture.protected();
    let identifier = write_user(&fixture, &admin);
    write_secret(&fixture, &admin);
    fixture.identity("reader");
    grant_reader(&fixture, &admin);
    let packed = json(&fixture.as_admin(
        &admin,
        &["pack", "app.vdbx", "--out", "copy.vdbx", "--sealed"],
    ));
    assert!(packed["sealed"].is_object(), "{packed}");
    let bytes = fs::read(fixture.0.join("copy.vdbx")).unwrap();
    assert!(!contains(&bytes, CANARY));
    assert!(!contains(&bytes, "WOMBAT-SECRET"));
    let got = json(&fixture.run(&as_reader(
        &admin,
        &["get", "users", &identifier, "--root", "copy.vdbx", "--json"],
    )));
    assert_eq!(got[&identifier]["name"], CANARY);
    let secrets = fixture.run(&as_reader(
        &admin,
        &["sql", "SELECT * FROM secrets", "--root", "copy.vdbx"],
    ));
    assert!(!contains(&secrets.stdout, "WOMBAT-SECRET"));
    let all = stdout(&fixture.as_admin(
        &admin,
        &["sql", "SELECT * FROM secrets", "--root", "copy.vdbx"],
    ));
    assert!(all.contains("WOMBAT-SECRET"), "{all}");
    let write = failure(&fixture.as_admin(
        &admin,
        &[
            "sql",
            "INSERT INTO users (name) VALUES ('x')",
            "--root",
            "copy.vdbx",
        ],
    ));
    assert!(!write.is_empty());
    let listed = json(&fixture.as_admin(&admin, &["access", "list", "copy.vdbx"]));
    assert_eq!(listed["policy"]["version"], 2);
    let report = json(&fixture.as_admin(&admin, &["verify", "copy.vdbx"]));
    assert_eq!(report["ok"], true, "{report}");
}

#[test]
fn a_sealed_pack_of_a_protected_database_needs_an_admin() {
    let fixture = Fixture::new("sealed-protected-reader");
    let admin = fixture.protected();
    write_user(&fixture, &admin);
    fixture.identity("reader");
    grant_reader(&fixture, &admin);
    let refused = failure(&fixture.run(&as_reader(
        &admin,
        &["pack", "app.vdbx", "--out", "copy.vdbx", "--sealed"],
    )));
    assert!(refused.contains("EACCES"), "{refused}");
    assert!(!fixture.0.join("copy.vdbx").exists());
}

#[test]
fn a_sealed_pack_of_a_protected_database_that_stops_leaves_no_copy() {
    let fixture = Fixture::new("sealed-protected-stopped");
    let admin = fixture.protected();
    let identifier = write_user(&fixture, &admin);
    let stopped = Command::new(BINARY)
        .current_dir(&fixture.0)
        .args([
            "pack",
            "app.vdbx",
            "--out",
            "copy.vdbx",
            "--sealed",
            "--identity",
            "file:admin.pem",
            "--trust-anchor",
            &admin,
        ])
        .env_remove("FYLO_IDENTITY")
        .env_remove("FYLO_TRUST_ANCHOR")
        .env("FYLO_RUST_FAILPOINT", "after-pack-staging")
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .output()
        .unwrap();
    assert!(!stopped.status.success(), "{stopped:?}");
    assert!(!fixture.0.join("copy.vdbx").exists());
    json(&fixture.as_admin(
        &admin,
        &["pack", "app.vdbx", "--out", "copy.vdbx", "--sealed"],
    ));
    let got = json(&fixture.as_admin(
        &admin,
        &["get", "users", &identifier, "--root", "copy.vdbx", "--json"],
    ));
    assert_eq!(got[&identifier]["name"], CANARY);
}

#[test]
fn access_audit_lists_every_change_with_its_signer() {
    let fixture = Fixture::new("audit");
    let admin = fixture.protected();
    write_user(&fixture, &admin);
    let reader = fixture.identity("reader");
    grant_reader(&fixture, &admin);
    json(&fixture.as_admin(
        &admin,
        &["access", "revoke", "app.vdbx", "--rule", "readers"],
    ));
    json(&fixture.as_admin(&admin, &["rekey", "app.vdbx", "--collection", "users"]));
    let audit = json(&fixture.as_admin(&admin, &["access", "audit", "app.vdbx"]));
    assert_eq!(audit["trustAnchor"], admin.as_str());
    let entries = audit["entries"].as_array().unwrap();
    let kinds: Vec<&str> = entries
        .iter()
        .map(|entry| entry["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["create", "grant", "revoke", "rekeyCollection"]);
    assert!(
        entries
            .iter()
            .all(|entry| entry["signer"] == admin.as_str())
    );
    assert_eq!(entries[0]["added"][0], "root-admin");
    assert_eq!(entries[1]["rule"], "readers");
    assert_eq!(entries[1]["added"][0], "readers");
    assert_eq!(entries[2]["removed"][0], "readers");
    assert_eq!(entries[3]["collection"], "users");
    assert!(entries[1]["at"].as_u64().unwrap() > 0);
    // Another identity the policy names reads the audit too; the reader,
    // revoked, does not.
    let refused = failure(&fixture.run(&as_reader(&admin, &["access", "audit", "app.vdbx"])));
    assert!(
        refused.contains("EACCES") || refused.contains("no grant"),
        "{refused}"
    );
    assert!(!reader.is_empty());
}

#[test]
fn access_audit_refuses_a_database_opened_without_an_identity() {
    let fixture = Fixture::new("audit-locked");
    fixture.protected();
    let refused = failure(&fixture.run(&["access", "audit", "app.vdbx"]));
    assert!(refused.contains("open it with an identity"), "{refused}");
}
