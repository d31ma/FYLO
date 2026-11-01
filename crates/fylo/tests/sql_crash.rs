//! SQL crash matrix: interrupt a statement's commit and a `COMMIT` at every
//! durable transition of FYLO's write-ahead log, then reopen. Documents stay
//! the truth: the collection holds exactly the state before or exactly the
//! state after the interrupted commit, never a mix, and its index equals an
//! independent rebuild from the documents.
//!
//! Each case runs the SQL in a child copy of this test binary with
//! `FYLO_RUST_FAILPOINT` set, so `abort` really loses the process.
#![cfg(feature = "sql")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo::{OpenOptions, Query, Root};
use fylo_engine::{ReadOnlyEngine, WriteEngine};
use fylo_storage_native::{CollectionKind, NativeWriteRoot};
use serde_json::{Value, json};

const CHILD_ROOT: &str = "FYLO_SQL_CRASH_ROOT";
const CHILD_WORKLOAD: &str = "FYLO_SQL_CRASH_WORKLOAD";
const CHILD_TEST: &str = "sql_crash_child";
const FINISHED: &str = "FYLO-SQL-CRASH-CHILD-FINISHED";

/// The SQL each workload runs: one autocommit statement over many rows, and
/// an explicit transaction that inserts, updates and deletes.
fn workload(name: &str) -> &'static [&'static str] {
    match name {
        "statement" => &["UPDATE products SET price = price + 1000 WHERE price < 10"],
        "commit" => &[
            "BEGIN",
            "INSERT INTO products (doc) VALUES ('{\"name\":\"new-1\",\"price\":1}'), ('{\"name\":\"new-2\",\"price\":2}')",
            "UPDATE products SET price = 999 WHERE name = 'item-0001'",
            "DELETE FROM products WHERE name = 'item-0002'",
            "COMMIT",
        ],
        other => panic!("unknown workload {other}"),
    }
}

/// Runs only as the child: the parent sets the environment.
#[test]
fn sql_crash_child() {
    let (Ok(root), Ok(name)) = (std::env::var(CHILD_ROOT), std::env::var(CHILD_WORKLOAD)) else {
        return;
    };
    let root = Arc::new(Root::open(root, OpenOptions::default()).unwrap());
    let sql = root.sql().unwrap();
    for statement in workload(&name) {
        if let Err(error) = sql.execute(statement, &[]) {
            println!("CHILD-ERROR {statement}: {error}");
            let _ = sql.execute("ROLLBACK", &[]);
            break;
        }
    }
    println!("{FINISHED}");
}

struct Case {
    parent: PathBuf,
    root: PathBuf,
}

impl Case {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = std::env::temp_dir().join(format!(
            "fylo-sql-crash-{}-{}-{label}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&parent);
        let root = parent.join("root");
        let schema = parent.join("schemas").join("products");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(schema.join("history")).unwrap();
        fs::write(
            schema.join("history/v1.schema.json"),
            serde_json::to_vec(&json!({"name": "^.+$", "price?": "^-?[0-9.]+$"})).unwrap(),
        )
        .unwrap();
        fs::write(
            schema.join("manifest.json"),
            serde_json::to_vec(&json!({"current": "v1", "versions": [{"v": "v1"}]})).unwrap(),
        )
        .unwrap();
        NativeWriteRoot::open(&root)
            .unwrap()
            .create_collection("products", CollectionKind::Document, None)
            .unwrap();
        let engine = WriteEngine::open(&root).unwrap();
        engine
            .schema_push("products", Some(&parent.join("schemas")), None)
            .unwrap();
        let writer = NativeWriteRoot::open(&root).unwrap();
        for n in 0..40 {
            let id = writer.allocate_identifier().unwrap();
            engine
                .put_document(
                    "products",
                    &id,
                    json!({"name": format!("item-{n:04}"), "price": n})
                        .as_object()
                        .unwrap()
                        .clone(),
                    fylo_storage_native::WriteAccess::default(),
                )
                .unwrap();
        }
        let root = fs::canonicalize(root).unwrap();
        Self { parent, root }
    }

    /// Run `workload` in a child process with `failpoint` armed.
    fn run(&self, name: &str, failpoint: Option<(&str, &str, u64)>) -> (bool, String) {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads", "1"])
            .env(CHILD_ROOT, &self.root)
            .env(CHILD_WORKLOAD, name);
        if let Some((point, action, skip)) = failpoint {
            command
                .env("FYLO_RUST_FAILPOINT", point)
                .env("FYLO_RUST_FAILPOINT_ACTION", action)
                .env("FYLO_RUST_FAILPOINT_SKIP", skip.to_string());
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        (stdout.contains(FINISHED), stdout)
    }

    /// The collection as a sorted list of documents, after reopening the root
    /// and recovering the collection.
    fn documents(&self) -> Vec<String> {
        let root = Root::open(&self.root, OpenOptions::default()).unwrap();
        let mut documents: Vec<String> = root
            .collection("products")
            .find(&Query::from_json(&json!({})).unwrap())
            .unwrap()
            .into_iter()
            .map(|(_, document)| Value::Object(document).to_string())
            .collect();
        documents.sort();
        root.close().unwrap();
        documents
    }

    fn recover(&self) {
        NativeWriteRoot::open(&self.root)
            .unwrap()
            .recover_collection("products")
            .unwrap();
    }

    /// The index matches an independent rebuild from the documents, and a
    /// rebuild changes no indexed answer.
    fn assert_index_rebuilds(&self, label: &str) {
        let report = ReadOnlyEngine::open(&self.root)
            .unwrap()
            .verify_index("products")
            .unwrap();
        assert!(report.rebuild_equivalent, "{label}: {report:?}");
        assert_eq!(report.indexed_documents, report.live_documents, "{label}");
        let indexed = |path: &Path| {
            let root = Arc::new(Root::open(path, OpenOptions::default()).unwrap());
            let sql = root.sql().unwrap();
            let rows = sql
                .query("SELECT id FROM products WHERE price > 900 ORDER BY id", &[])
                .unwrap()
                .rows;
            drop(sql);
            Arc::into_inner(root).unwrap().close().unwrap();
            rows
        };
        let before = indexed(&self.root);
        NativeWriteRoot::open(&self.root)
            .unwrap()
            .rebuild_collection("products")
            .unwrap();
        assert_eq!(
            indexed(&self.root),
            before,
            "{label}: rebuild changed an answer"
        );
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

/// The state before and after `workload` runs uninterrupted.
fn expected(name: &str) -> (Vec<String>, Vec<String>) {
    let case = Case::new(&format!("baseline-{name}"));
    let before = case.documents();
    let (finished, output) = case.run(name, None);
    assert!(finished, "baseline {name} did not finish: {output}");
    let after = case.documents();
    assert_ne!(before, after, "baseline {name} changed nothing");
    (before, after)
}

#[test]
fn an_interrupted_sql_commit_leaves_the_documents_before_or_after_and_the_index_rebuildable() {
    if std::env::var(CHILD_ROOT).is_ok() {
        return;
    }
    // (failpoint, hits to pass, whether the commit is durable by then)
    let points: [(&str, u64, bool); 6] = [
        ("before-log-append", 0, false),
        ("after-log-append", 0, false),
        ("before-log-sync", 0, false),
        ("after-log-sync", 0, true),
        ("after-log-apply-step", 0, true),
        ("after-log-apply-step", 2, true),
    ];
    let mut summary = BTreeMap::new();
    for name in ["statement", "commit"] {
        let (before, after) = expected(name);
        for (point, skip, durable) in points {
            for action in ["abort", "enospc"] {
                let label = format!("{name}/{point}+{skip}/{action}");
                let case = Case::new(&label.replace('/', "-"));
                let (finished, output) = case.run(name, Some((point, action, skip)));
                let interrupted = !finished || output.contains("CHILD-ERROR");
                assert!(
                    interrupted,
                    "{label}: the failpoint was never reached: {output}"
                );
                case.recover();
                let state = case.documents();
                let outcome = if state == before {
                    "before"
                } else if state == after {
                    "after"
                } else {
                    panic!("{label}: a mix of before and after\n{state:#?}");
                };
                if action == "abort" && durable {
                    assert_eq!(outcome, "after", "{label}: a durable commit was lost");
                }
                if action == "enospc" && !durable {
                    assert_eq!(outcome, "before", "{label}: a failed commit left writes");
                }
                case.assert_index_rebuilds(&label);
                summary.insert(label, outcome);
            }
        }
    }
    for (label, outcome) in &summary {
        println!("{label}: {outcome}");
    }
}
