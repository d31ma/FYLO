use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Barrier;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_engine::{EngineErrorCode, ReadOnlyEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{
    CollectionKind, NativeRoot, NativeStorageErrorCode, NativeWriteRoot, PutDocumentOptions,
};
use serde_json::json;

struct Fixture(PathBuf);

impl Fixture {
    const ID: &str = "4VRNF52J040";

    fn new() -> Self {
        Self::with_score(0)
    }

    fn with_score(score: usize) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fylo-index-reuse-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self(root);
        fixture.seed(score);
        fixture
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(&self.0).unwrap()
    }

    fn document_path(&self) -> PathBuf {
        NativeRoot::open(&self.0)
            .unwrap()
            .collection("records")
            .unwrap()
            .read_document(Self::ID)
            .unwrap()
            .path
    }

    fn seed(&self, score: usize) {
        let writer = self.writer();
        writer
            .create_collection("records", CollectionKind::Document, None)
            .unwrap();
        writer
            .put_document(
                "records",
                Self::ID,
                &serde_json::to_vec(&json!({"score":score,"mirror":score})).unwrap(),
                PutDocumentOptions::default(),
            )
            .unwrap();
    }

    fn query(score: usize) -> StructuredQuery {
        StructuredQuery::from_value(
            &json!({"$ops":[{"score":{"$eq":score}}]}),
            QueryLimits::default(),
        )
        .unwrap()
    }

    fn assert_score(engine: &ReadOnlyEngine, score: usize) {
        let rows = engine.find("records", &Self::query(score)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].metadata.id, Self::ID);
        assert_eq!(rows[0].document.fields()["score"], json!(score));
        assert_eq!(rows[0].document.fields()["mirror"], json!(score));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn concurrent_queries_and_rebuilds_preserve_coherent_rows_and_settled_freshness() {
    let fixture = Fixture::new();
    let barrier = Barrier::new(5);
    std::thread::scope(|scope| {
        let readers = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let engine = ReadOnlyEngine::open(&fixture.0).unwrap();
                    let query = StructuredQuery::from_value(
                        &json!({"$ops":[{"score":{"$eq":0}},{"score":{"$eq":1}}]}),
                        QueryLimits::default(),
                    )
                    .unwrap();
                    Fixture::assert_score(&engine, 0);
                    let mut successes = 0;
                    let mut failures = Vec::new();
                    for step in 0..24 {
                        barrier.wait();
                        for _ in 0..8 {
                            match engine.find("records", &query) {
                                Ok(rows) => {
                                    if rows.len() == 1 {
                                        let fields = rows[0].document.fields();
                                        if fields["score"] != fields["mirror"] {
                                            failures.push(format!("incoherent row {fields:?}"));
                                        }
                                    } else {
                                        failures.push(format!("row count {}", rows.len()));
                                    }
                                    successes += 1;
                                }
                                Err(error) => {
                                    let changed_path = error.storage_code()
                                        == Some(NativeStorageErrorCode::UnsafePath)
                                        && error.to_string().contains(
                                            "storage path changed while an open file was being read",
                                        );
                                    if error.code() != EngineErrorCode::ConcurrentWrite
                                        && !changed_path
                                    {
                                        failures.push(format!("{error:?}"));
                                    }
                                }
                            }
                        }
                        barrier.wait();
                        if catch_unwind(AssertUnwindSafe(|| {
                            Fixture::assert_score(&engine, (step + 1) % 2);
                        }))
                        .is_err()
                        {
                            failures.push(format!("settled read failed after step {step}"));
                        }
                    }
                    (successes, failures)
                })
            })
            .collect::<Vec<_>>();
        let writer = fixture.writer();
        let mut write_failures = Vec::new();
        for step in 0..24 {
            barrier.wait();
            let score = (step + 1) % 2;
            if catch_unwind(AssertUnwindSafe(|| {
                writer
                    .patch_document(
                        "records",
                        Fixture::ID,
                        &serde_json::to_vec(&json!({"score":score,"mirror":score})).unwrap(),
                        None,
                    )
                    .unwrap();
                if step % 3 == 0 {
                    writer.rebuild_collection("records").unwrap();
                }
            }))
            .is_err()
            {
                write_failures.push(step);
            }
            barrier.wait();
        }
        let results = readers
            .into_iter()
            .map(|reader| reader.join().unwrap())
            .collect::<Vec<_>>();
        let successful_reads = results
            .iter()
            .map(|(successes, _)| successes)
            .sum::<usize>();
        assert!(successful_reads > 0);
        assert!(write_failures.is_empty(), "{write_failures:?}");
        assert!(
            results.iter().all(|(_, errors)| errors.is_empty()),
            "{results:?}"
        );
    });
}

#[test]
fn warm_queries_follow_drop_recreate_rebuild_delete_and_restore() {
    let fixture = Fixture::new();
    let engine = ReadOnlyEngine::open(&fixture.0).unwrap();
    Fixture::assert_score(&engine, 0);
    fixture.writer().drop_collection("records").unwrap();
    fixture.seed(1);
    Fixture::assert_score(&engine, 1);
    assert!(
        engine
            .find("records", &Fixture::query(0))
            .unwrap()
            .is_empty()
    );
    let writer = fixture.writer();
    writer.rebuild_collection("records").unwrap();
    Fixture::assert_score(&engine, 1);
    writer
        .delete_document("records", Fixture::ID, None)
        .unwrap();
    assert!(
        engine
            .find("records", &Fixture::query(1))
            .unwrap()
            .is_empty()
    );
    writer
        .restore_document("records", Fixture::ID, None)
        .unwrap();
    Fixture::assert_score(&engine, 1);
}

#[test]
fn retained_engine_observes_replaced_root_with_equal_generation() {
    let original = Fixture::new();
    let replacement = Fixture::with_score(1);
    let engine = ReadOnlyEngine::open(&original.0).unwrap();
    Fixture::assert_score(&engine, 0);
    let old_generation = engine.inspect("records").unwrap().generation;
    let new_generation = ReadOnlyEngine::open(&replacement.0)
        .unwrap()
        .inspect("records")
        .unwrap()
        .generation;
    assert_eq!(old_generation, new_generation);
    let displaced = replacement.0.join("previous-root");
    fs::rename(&original.0, &displaced).unwrap();
    fs::create_dir(&original.0).unwrap();
    for entry in fs::read_dir(&replacement.0).unwrap() {
        let entry = entry.unwrap();
        if entry.path() != displaced {
            fs::rename(entry.path(), original.0.join(entry.file_name())).unwrap();
        }
    }
    Fixture::assert_score(&engine, 1);
    assert!(
        engine
            .find("records", &Fixture::query(0))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unchanged_generation_still_reports_permanent_document_corruption() {
    let fixture = Fixture::new();
    let engine = ReadOnlyEngine::open(&fixture.0).unwrap();
    Fixture::assert_score(&engine, 0);
    fs::write(fixture.document_path(), b"{broken").unwrap();
    let error = engine.find("records", &Fixture::query(0)).unwrap_err();
    assert_eq!(error.code(), EngineErrorCode::Format);
}

#[cfg(unix)]
#[test]
fn unchanged_generation_still_rejects_a_document_symlink() {
    let fixture = Fixture::new();
    let engine = ReadOnlyEngine::open(&fixture.0).unwrap();
    Fixture::assert_score(&engine, 0);
    let path = fixture.document_path();
    let displaced = fixture.0.join("displaced.json");
    fs::rename(&path, &displaced).unwrap();
    std::os::unix::fs::symlink(&displaced, &path).unwrap();
    let error = engine.find("records", &Fixture::query(0)).unwrap_err();
    assert_eq!(
        error.storage_code(),
        Some(NativeStorageErrorCode::UnsafePath)
    );
}
