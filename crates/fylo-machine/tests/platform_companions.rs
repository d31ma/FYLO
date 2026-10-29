//! A root on a volume where macOS writes an `AppleDouble` companion,
//! `._<name>`, beside every file and directory that carries an extended
//! attribute, and Finder a `.DS_Store` in every directory it shows. Every
//! invocation first walks the root's collections to recover them, so each
//! one has to read the companions as the platform's.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, InvocationOptions, InvocationRuntime};
use fylo_storage_native::RootOpenOptions;
use serde_json::{Value, json};

const NOTES: &str = "notes";
const APPLE_DOUBLE: &[u8] = b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        ";

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-machine-companions-{}-{stamp}",
            std::process::id()
        ));
        let root = parent.join("root");
        fs::create_dir_all(&root).unwrap();
        Self { parent, root }
    }

    /// A fresh invocation runtime: each one reopens and recovers the root.
    fn runtime(&self) -> InvocationRuntime {
        let options = InvocationOptions::new(
            RootOpenOptions::new(&self.root),
            ApplicationConfig::default(),
            FrameLimits::default(),
            (
                Instant::now() + Duration::from_secs(30),
                Duration::from_millis(100),
            ),
        )
        .unwrap();
        InvocationRuntime::new(options).unwrap()
    }

    fn success(&self, request: &Value) -> Value {
        let mut frame = serde_json::to_vec(request).unwrap();
        frame.push(b'\n');
        let response: Value =
            serde_json::from_slice(&self.runtime().execute(&frame).unwrap()).unwrap();
        assert_eq!(response["ok"], true, "{request}: {response}");
        response["result"].clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

/// Write a companion beside every file and directory under `directory`, and
/// Finder's state in every directory.
fn plant(directory: &Path) {
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("._") || name == ".DS_Store" {
            continue;
        }
        fs::write(directory.join(format!("._{name}")), APPLE_DOUBLE).unwrap();
        if entry.file_type().unwrap().is_dir() {
            plant(&entry.path());
        }
    }
    fs::write(directory.join(".DS_Store"), b"\x00\x00\x00\x01Bud1").unwrap();
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 40_000 + number)
}

fn found(fixture: &Fixture) -> Vec<String> {
    let result = fixture.success(&json!({"op":"findDocs","collection":NOTES,"query":{}}));
    let mut ids: Vec<String> = result.as_object().unwrap().keys().cloned().collect();
    ids.sort();
    ids
}

#[test]
fn invocations_work_on_a_root_among_companions() {
    let fixture = Fixture::new();
    fixture.success(&json!({"op":"createCollection","collection":NOTES}));
    for number in 0..3 {
        fixture.success(&json!({
            "op":"putData","collection":NOTES,"id":identifier(number),"data":{"n":number}
        }));
    }
    plant(&fixture.root);

    assert_eq!(
        found(&fixture),
        [identifier(0), identifier(1), identifier(2)]
    );
    let read = fixture.success(&json!({"op":"getDoc","collection":NOTES,"id":identifier(1)}));
    assert_eq!(read[identifier(1)]["n"], 1, "{read}");
    fixture.success(&json!({
        "op":"patchDoc","collection":NOTES,"id":identifier(1),"newDoc":{"n":10}
    }));
    fixture.success(&json!({"op":"delDoc","collection":NOTES,"id":identifier(0)}));
    plant(&fixture.root);
    fixture.success(&json!({"op":"rebuildCollection","collection":NOTES}));
    assert_eq!(found(&fixture), [identifier(1), identifier(2)]);

    fixture.success(&json!({
        "op":"migrateLayout","collection":NOTES,"layout":"segments","allowEngineOnlyAccess":true
    }));
    plant(&fixture.root);
    fixture.success(&json!({"op":"compactCollection","collection":NOTES}));
    assert_eq!(found(&fixture), [identifier(1), identifier(2)]);

    fixture.success(&json!({"op":"dropCollection","collection":NOTES}));
    let collections = fixture.root.join(".collections");
    assert!(!collections.join("._notes").exists());
    fixture.success(&json!({"op":"createCollection","collection":NOTES}));
    assert!(found(&fixture).is_empty());
}
