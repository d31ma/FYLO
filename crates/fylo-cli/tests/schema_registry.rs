//! `fylo schema push` and `pull` from the command line, and a push
//! interrupted between writing its version file and moving the descriptor's
//! head (ADR 0017 section 3).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fylo_storage_native::{CollectionKind, NativeRoot, NativeWriteRoot};
use serde_json::{Value, json};

const BINARY: &str = env!("CARGO_BIN_EXE_fylo-rust");
const WRITE_PREVIEW: &str = env!("CARGO_BIN_EXE_fylo-write-preview");

struct Fixture(PathBuf);

impl Fixture {
    /// A root with an `items` collection and a schema folder whose `items`
    /// have `v1`.
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-cli-schema-registry-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("root")).unwrap();
        let fixture = Self(fs::canonicalize(path).unwrap());
        let writer = NativeWriteRoot::open(fixture.root()).unwrap();
        writer
            .create_collection("items", CollectionKind::Document, None)
            .unwrap();
        // The root has had its one-time import of `fylo.schema`, so the
        // folder's versions wait for an explicit push.
        writer
            .write_schema_import_record(br#"{"format":"fylo.schema-import.v1"}"#)
            .unwrap();
        let folder = fixture.schemas().join("items");
        fs::create_dir_all(folder.join("history")).unwrap();
        fs::write(
            folder.join("manifest.json"),
            br#"{"current":"v1","versions":[{"v":"v1"}]}"#,
        )
        .unwrap();
        fs::write(
            folder.join("history/v1.schema.json"),
            br#"{"title":"^.+$"}"#,
        )
        .unwrap();
        fixture
    }

    fn root(&self) -> PathBuf {
        self.0.join("root")
    }

    fn schemas(&self) -> PathBuf {
        self.0.join("schemas")
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(BINARY)
            .current_dir(&self.0)
            .args(arguments)
            .output()
            .unwrap()
    }

    fn push_with_failpoint(&self, action: &str) -> Output {
        Command::new(WRITE_PREVIEW)
            .args(["push-schema", "--root"])
            .arg(self.root())
            .args(["--collection", "items", "--schema-dir"])
            .arg(self.schemas())
            .env("FYLO_RUST_FAILPOINT", "after-schema-file")
            .env("FYLO_RUST_FAILPOINT_ACTION", action)
            .output()
            .unwrap()
    }

    fn head(&self) -> Option<u32> {
        NativeRoot::open(self.root())
            .unwrap()
            .schema_registry_head("items")
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn push_and_pull_from_the_command_line() {
    let fixture = Fixture::new("cli");
    let root = fixture.root();
    let schemas = fixture.schemas();
    let pushed = fixture.run(&[
        "schema",
        "push",
        "items",
        "--root",
        path(&root),
        "--schema-dir",
        path(&schemas),
    ]);
    assert!(pushed.status.success(), "{pushed:?}");
    assert!(
        text(&pushed).starts_with("Pushed 1 schema version(s) of items: v1 (head v1)"),
        "{pushed:?}"
    );
    assert_eq!(fixture.head(), Some(1));

    let again = fixture.run(&[
        "schema",
        "push",
        "items",
        "--root",
        path(&root),
        "--schema-dir",
        path(&schemas),
        "--json",
    ]);
    assert!(again.status.success(), "{again:?}");
    let report: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(report["pushed"], json!([]), "{report}");
    assert_eq!(report["head"], "v1", "{report}");

    let pulled = fixture.run(&[
        "schema",
        "pull",
        "items",
        "--root",
        path(&root),
        "--out",
        "pulled",
    ]);
    assert!(pulled.status.success(), "{pulled:?}");
    assert!(
        text(&pulled).starts_with("Pulled 1 schema version(s) of items to "),
        "{pulled:?}"
    );
    let schema: Value = serde_json::from_slice(
        &fs::read(fixture.0.join("pulled/items/history/v1.schema.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(schema, json!({"title":"^.+$"}));

    let missing_out = fixture.run(&["schema", "pull", "items", "--root", path(&root)]);
    assert!(!missing_out.status.success(), "{missing_out:?}");
}

#[test]
fn a_push_interrupted_after_its_version_file_leaves_the_old_head() {
    let fixture = Fixture::new("crash");
    for action in ["enospc", "abort"] {
        let interrupted = fixture.push_with_failpoint(action);
        assert!(!interrupted.status.success(), "{action}: {interrupted:?}");
        // The version file is durable, but the descriptor never named it, so
        // the collection has no registry and reads nothing from the file.
        assert!(fixture.root().join(".fylo-schemas/items/1.json").exists());
        assert_eq!(fixture.head(), None, "{action}");
        let descriptor: Value = serde_json::from_slice(
            &fs::read(fixture.root().join(".fylo-catalog/collections/items.json")).unwrap(),
        )
        .unwrap();
        assert!(descriptor.get("schemaHead").is_none(), "{descriptor}");
    }
    // The next push replaces the stray file and commits it.
    let output = Command::new(WRITE_PREVIEW)
        .args(["push-schema", "--root"])
        .arg(fixture.root())
        .args(["--collection", "items", "--schema-dir"])
        .arg(fixture.schemas())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.head(), Some(1));
}
