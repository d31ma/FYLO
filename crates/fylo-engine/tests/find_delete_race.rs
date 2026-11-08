//! A query that runs while documents are created and deleted answers from one
//! stable generation: it never fails because a listed document was deleted
//! before it was read (issue #104).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use fylo_engine::{EngineError, HotSet, ReadDocument, ReadOnlyEngine, WriteEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{CollectionKind, NativeStorageErrorCode, NativeWriteRoot};
use serde_json::{Map, Value, json};

const SEEDED: usize = 20;
const CHURN_CYCLES: usize = 150;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fylo-find-delete-race-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self(root);
        NativeWriteRoot::open(&fixture.0)
            .unwrap()
            .create_collection("records", CollectionKind::Document, None)
            .unwrap();
        let writer = WriteEngine::open(&fixture.0).unwrap();
        for number in 0..SEEDED {
            Self::put(&writer, number);
        }
        fixture
    }

    fn id(number: usize) -> String {
        format!("4VRNF5{:05}", 30_000 + number)
    }

    fn put(writer: &WriteEngine, number: usize) {
        let fields: Map<String, Value> = json!({"number": number, "body": "x".repeat(256)})
            .as_object()
            .unwrap()
            .clone();
        writer
            .put_document("records", &Self::id(number), fields)
            .unwrap();
    }

    /// Create and delete one short-lived document per cycle, clearing the hot
    /// set after each write as an exclusive session does.
    fn churn(&self, hot_set: Option<&HotSet>, done: &AtomicBool) {
        let _finished = Finished(done);
        let writer = WriteEngine::open(&self.0).unwrap();
        let storage = NativeWriteRoot::open(&self.0).unwrap();
        for cycle in 0..CHURN_CYCLES {
            let number = SEEDED + cycle;
            Self::put(&writer, number);
            hot_set.inspect(|hot_set| hot_set.clear());
            storage
                .delete_document("records", &Self::id(number))
                .unwrap();
            hot_set.inspect(|hot_set| hot_set.clear());
        }
    }

    /// Query every document until the churn ends; returns how many queries
    /// ran. A query may still report a concurrent write once its stable-read
    /// attempts run out, which is the documented, retryable outcome.
    fn query_until(engine: &ReadOnlyEngine, done: &AtomicBool) -> usize {
        let mut attempted = 0;
        while !done.load(Ordering::SeqCst) {
            match engine.find("records", &everything()) {
                Ok(rows) => assert_seeded_rows_present(&rows),
                Err(error) => assert_not_missing(&error),
            }
            attempted += 1;
        }
        attempted
    }

    fn race(&self, engine: &ReadOnlyEngine, hot_set: Option<&HotSet>) {
        let done = AtomicBool::new(false);
        let attempted = thread::scope(|scope| {
            let reader = scope.spawn(|| Self::query_until(engine, &done));
            self.churn(hot_set, &done);
            reader.join().unwrap()
        });
        assert!(attempted > 0, "no query ran during the churn");
        let settled = engine.find("records", &everything()).unwrap();
        assert_eq!(settled.len(), SEEDED);
    }
}

/// Ends the reader's loop when the churn ends, even by a panic.
struct Finished<'a>(&'a AtomicBool);

impl Drop for Finished<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn everything() -> StructuredQuery {
    StructuredQuery::from_value(&json!({}), QueryLimits::default()).unwrap()
}

fn assert_seeded_rows_present(rows: &[ReadDocument]) {
    let seeded = rows
        .iter()
        .filter(|row| row.metadata.id <= Fixture::id(SEEDED - 1))
        .count();
    assert_eq!(seeded, SEEDED, "a query lost a document nobody deleted");
}

fn assert_not_missing(error: &EngineError) {
    assert_ne!(
        error.storage_code(),
        Some(NativeStorageErrorCode::NotFound),
        "a query surfaced a document deleted while it ran: {error}"
    );
}

#[test]
fn storage_queries_never_fail_on_a_document_deleted_while_they_run() {
    let fixture = Fixture::new();
    let engine = ReadOnlyEngine::open(&fixture.0).unwrap();
    fixture.race(&engine, None);
}

#[test]
fn hot_set_queries_never_fail_on_a_document_deleted_while_they_run() {
    let fixture = Fixture::new();
    let hot_set = Arc::new(HotSet::new(HotSet::DEFAULT_BUDGET_BYTES));
    let engine = ReadOnlyEngine::open(&fixture.0)
        .unwrap()
        .with_hot_set(Arc::clone(&hot_set));
    fixture.race(&engine, Some(&hot_set));
}
