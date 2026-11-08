//! Opening a root after a crash finishes the interrupted commit before the
//! first read, as the machine protocol does when `fylo exec` starts.
//!
//! The crash runs in a child copy of this test binary with
//! `FYLO_RUST_FAILPOINT` set, so `abort` really loses the process.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo::{OpenOptions, Query, Root};
use fylo_storage_native::NativeWriteRoot;
use serde_json::json;

const CHILD_ROOT: &str = "FYLO_OPEN_RECOVERY_ROOT";
const CHILD_TEST: &str = "open_recovery_child";
const FINISHED: &str = "FYLO-OPEN-RECOVERY-CHILD-FINISHED";

/// Runs only as the child: the parent sets the environment.
#[test]
fn open_recovery_child() {
    let Ok(root) = std::env::var(CHILD_ROOT) else {
        return;
    };
    let root = Root::open(root, OpenOptions::default()).unwrap();
    let document = json!({"name": "crashed"}).as_object().unwrap().clone();
    if let Err(error) = root.collection("people").put(document) {
        println!("CHILD-ERROR {error}");
    }
    println!("{FINISHED}");
}

struct Case(PathBuf);

impl Case {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fylo-open-recovery-{}-{}-{label}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        let path = fs::canonicalize(path).unwrap();
        let root = Root::open(&path, OpenOptions::default()).unwrap();
        root.create_collection("people").unwrap();
        for name in ["Ada", "Grace"] {
            let document = json!({ "name": name }).as_object().unwrap().clone();
            root.collection("people").put(document).unwrap();
        }
        root.close().unwrap();
        Self(path)
    }

    /// Put one document in a child process that `failpoint` aborts.
    fn crash(&self, failpoint: &str) -> String {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads", "1"])
            .env(CHILD_ROOT, &self.0)
            .env("FYLO_RUST_FAILPOINT", failpoint)
            .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            !stdout.contains(FINISHED),
            "{failpoint}: the failpoint was never reached: {stdout}"
        );
        stdout
    }

    /// The collection's names, as a fresh `Root::open` reads them first.
    fn names(&self) -> Vec<String> {
        let root = Root::open(&self.0, OpenOptions::default()).unwrap();
        let mut names: Vec<String> = root
            .collection("people")
            .find(&Query::from_json(&json!({})).unwrap())
            .unwrap()
            .into_iter()
            .map(|(_, document)| document["name"].as_str().unwrap().to_owned())
            .collect();
        names.sort();
        root.close().unwrap();
        names
    }

    /// Recover the collection as the machine protocol does at startup.
    fn recover_as_machine_protocol(&self) {
        NativeWriteRoot::open(&self.0)
            .unwrap()
            .recover_collection("people")
            .unwrap();
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn reopened_and_recovered(failpoint: &str) -> (Vec<String>, Vec<String>) {
    let case = Case::new(failpoint);
    case.crash(failpoint);
    let reopened = case.names();
    case.recover_as_machine_protocol();
    (reopened, case.names())
}

#[test]
fn reopening_after_a_crash_after_the_log_sync_keeps_the_durable_put() {
    if std::env::var(CHILD_ROOT).is_ok() {
        return;
    }
    let (reopened, recovered) = reopened_and_recovered("after-log-sync");
    assert_eq!(recovered, ["Ada", "Grace", "crashed"]);
    assert_eq!(reopened, recovered);
}

#[test]
fn reopening_after_a_crash_mid_apply_keeps_the_durable_put() {
    if std::env::var(CHILD_ROOT).is_ok() {
        return;
    }
    let (reopened, recovered) = reopened_and_recovered("after-log-apply-step");
    assert_eq!(recovered, ["Ada", "Grace", "crashed"]);
    assert_eq!(reopened, recovered);
}

#[test]
fn reopening_after_a_crash_before_the_log_append_drops_the_put() {
    if std::env::var(CHILD_ROOT).is_ok() {
        return;
    }
    let (reopened, recovered) = reopened_and_recovered("before-log-append");
    assert_eq!(recovered, ["Ada", "Grace"]);
    assert_eq!(reopened, recovered);
}
