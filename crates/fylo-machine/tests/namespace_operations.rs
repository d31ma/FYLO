//! `createNamespace`, `listNamespaces` and `dropNamespace` (ADR 0033 section
//! 3) on an app folder in the new layout.

use std::io::Cursor;
use std::path::PathBuf;

use fylo_machine::{ApplicationConfig, FrameLimits, serve_application};
use serde_json::{Value, json};

struct Drive {
    folder: PathBuf,
}

impl Drive {
    fn new() -> Self {
        let folder = fylo_test_support::TestFolder::path().join(format!(
            "fylo-namespace-ops-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&folder).unwrap();
        Self {
            folder: std::fs::canonicalize(folder).unwrap(),
        }
    }

    fn app(&self) -> PathBuf {
        self.folder.join("chat")
    }

    /// Answer one request per value, in order, with `root` set to the app.
    fn frames(&self, requests: &[Value]) -> Vec<Value> {
        let input = requests
            .iter()
            .map(|request| {
                let mut request = request.clone();
                request["root"] = json!(self.app());
                format!("{request}\n")
            })
            .collect::<String>();
        let mut reader = Cursor::new(input.into_bytes());
        let mut output = Vec::new();
        serve_application(
            &mut reader,
            &mut output,
            None,
            FrameLimits::default(),
            ApplicationConfig::default(),
            false,
            true,
        )
        .unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Drive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

#[test]
fn namespaces_are_created_listed_and_dropped() {
    let drive = Drive::new();
    let frames = drive.frames(&[
        json!({"op": "createNamespace", "namespace": "billing"}),
        json!({"op": "createNamespace", "namespace": "slice-1"}),
        json!({"op": "listNamespaces"}),
        json!({"op": "dropNamespace", "namespace": "slice-1"}),
        json!({"op": "listNamespaces"}),
    ]);
    assert_eq!(frames[0]["ok"], json!(true), "{}", frames[0]);
    assert_eq!(frames[0]["result"]["namespace"], json!("billing"));
    assert!(drive.app().join("billing/.fylo/namespace.json").is_file());
    assert_eq!(
        frames[2]["result"]["namespaces"],
        json!(["billing", "slice-1"])
    );
    assert_eq!(frames[3]["result"]["dropped"], json!(true), "{}", frames[3]);
    assert_eq!(frames[4]["result"]["namespaces"], json!(["billing"]));
}

#[test]
fn an_existing_namespace_is_not_created_twice() {
    let drive = Drive::new();
    let frames = drive.frames(&[
        json!({"op": "createNamespace", "namespace": "billing"}),
        json!({"op": "createNamespace", "namespace": "billing"}),
    ]);
    assert_eq!(frames[1]["ok"], json!(false));
    assert_eq!(frames[1]["error"]["code"], json!("ENAMESPACEEXISTS"));
}

#[test]
fn a_namespace_with_collections_needs_cascade_to_be_dropped() {
    let drive = Drive::new();
    let created = drive.frames(&[json!({"op": "createNamespace", "namespace": "slice-1"})]);
    assert_eq!(created[0]["ok"], json!(true), "{}", created[0]);
    std::fs::create_dir(drive.app().join("slice-1/users")).unwrap();
    let frames = drive.frames(&[
        json!({"op": "dropNamespace", "namespace": "slice-1"}),
        json!({"op": "dropNamespace", "namespace": "slice-1", "cascade": true}),
    ]);
    assert_eq!(frames[0]["error"]["code"], json!("ENAMESPACENOTEMPTY"));
    assert_eq!(frames[1]["ok"], json!(true), "{}", frames[1]);
    assert!(!drive.app().join("slice-1").exists());
}

#[test]
fn dropping_a_missing_namespace_is_not_found() {
    let drive = Drive::new();
    let frames = drive.frames(&[
        json!({"op": "createNamespace", "namespace": "public"}),
        json!({"op": "dropNamespace", "namespace": "slice-9"}),
    ]);
    assert_eq!(frames[1]["error"]["code"], json!("ENATIVE_NOT_FOUND"));
}

#[test]
fn a_bad_namespace_name_is_refused() {
    let drive = Drive::new();
    let frames = drive.frames(&[json!({"op": "createNamespace", "namespace": "Bad Name"})]);
    assert_eq!(frames[0]["error"]["code"], json!("ENAMESPACE"));
}
