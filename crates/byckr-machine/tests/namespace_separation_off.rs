//! Until slice L1-5 namespaces do not keep data apart, so every namespace
//! but `public` is refused with `ENAMESPACEUNSUPPORTED` before anything is
//! written (ADR 0033 section 3). These tests never turn the switch on.

use std::io::Cursor;
use std::path::PathBuf;

use byckr_machine::{ApplicationConfig, FrameLimits, NamespaceName, serve_application};
use serde_json::{Value, json};

struct Root {
    folder: PathBuf,
}

impl Root {
    const UNSUPPORTED: &str = "ENAMESPACEUNSUPPORTED";

    fn new() -> Self {
        let folder = byckr_test_support::TestFolder::path().join(format!(
            "fylo-namespace-off-{}-{}",
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

    fn root(&self) -> PathBuf {
        self.folder.join("db")
    }

    /// Answer one request per value, in order, with `root` set, in a session
    /// whose default namespace is `session`.
    fn frames(&self, requests: &[Value], session: Option<&str>) -> Vec<Value> {
        let mut input = String::new();
        for request in requests {
            let mut request = request.clone();
            request["root"] = json!(self.root());
            input.push_str(&request.to_string());
            input.push('\n');
        }
        let application = ApplicationConfig {
            namespace: session.map(|name| NamespaceName::parse(name).unwrap()),
            ..ApplicationConfig::default()
        };
        let mut reader = Cursor::new(input.into_bytes());
        let mut output = Vec::new();
        serve_application(
            &mut reader,
            &mut output,
            None,
            FrameLimits::default(),
            application,
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

    fn holds_collection(&self, name: &str) -> bool {
        self.root().join(".collections").join(name).exists()
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

#[test]
fn a_request_in_another_namespace_is_refused_and_writes_nothing() {
    let root = Root::new();
    let frames = root.frames(
        &[json!({"op": "createCollection", "collection": "users", "namespace": "tenant-b"})],
        None,
    );
    assert_eq!(frames[0]["ok"], json!(false));
    assert_eq!(frames[0]["error"]["code"], json!(Root::UNSUPPORTED));
    let message = frames[0]["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("not yet supported in this build"),
        "{message}"
    );
    assert!(!root.holds_collection("users"));
}

#[test]
fn the_default_namespace_works_as_before() {
    let root = Root::new();
    let frames = root.frames(
        &[
            json!({"op": "createCollection", "collection": "users"}),
            json!({"op": "createCollection", "collection": "posts", "namespace": "public"}),
        ],
        None,
    );
    assert_eq!(frames[0]["ok"], json!(true), "{}", frames[0]);
    assert_eq!(frames[1]["ok"], json!(true), "{}", frames[1]);
    assert!(root.holds_collection("users"));
    assert!(root.holds_collection("posts"));
}

#[test]
fn a_session_in_another_namespace_is_refused() {
    let root = Root::new();
    let frames = root.frames(
        &[json!({"op": "createCollection", "collection": "users"})],
        Some("tenant-b"),
    );
    assert_eq!(frames[0]["error"]["code"], json!(Root::UNSUPPORTED));
    assert!(!root.holds_collection("users"));
}

#[test]
fn the_namespace_operations_are_refused() {
    let root = Root::new();
    let frames = root.frames(
        &[
            json!({"op": "createNamespace", "namespace": "public"}),
            json!({"op": "listNamespaces"}),
            json!({"op": "dropNamespace", "namespace": "public"}),
        ],
        None,
    );
    for frame in &frames {
        assert_eq!(frame["error"]["code"], json!(Root::UNSUPPORTED), "{frame}");
    }
    assert!(!root.root().join(".fylo").exists());
}
