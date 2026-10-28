//! A hot set answers reads exactly as storage does, from memory once warm,
//! within its budget, and never across a generation it did not read.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_engine::{EngineErrorCode, HotSet, ReadDocument, ReadOnlyEngine, WriteEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{CollectionKind, NativeWriteRoot, WriteAccess};
use serde_json::{Map, Value, json};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fylo-hot-set-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self(root);
        fixture
            .storage()
            .create_collection("records", CollectionKind::Document, None)
            .unwrap();
        fixture
    }

    fn storage(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(&self.0).unwrap()
    }

    fn writer(&self) -> WriteEngine {
        WriteEngine::open(&self.0).unwrap()
    }

    fn id(number: usize) -> String {
        format!("4VRNF5{:05}", 20_000 + number)
    }

    fn fields(value: &Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn put(&self, number: usize, value: &Value) {
        self.put_with_access(number, value, WriteAccess::default());
    }

    fn put_with_access(&self, number: usize, value: &Value, access: WriteAccess) {
        self.writer()
            .put_document("records", &Self::id(number), Self::fields(value), access)
            .unwrap();
    }

    fn patch(&self, number: usize, value: &Value) {
        self.writer()
            .patch_document("records", &Self::id(number), Self::fields(value), None)
            .unwrap();
    }

    fn seed(&self, count: usize) {
        for number in 0..count {
            self.put(
                number,
                &json!({"number": number, "group": number % 5, "body": "x".repeat(64)}),
            );
        }
    }

    fn cold(&self) -> ReadOnlyEngine {
        ReadOnlyEngine::open(&self.0).unwrap()
    }

    fn hot(&self, hot_set: &Arc<HotSet>) -> ReadOnlyEngine {
        self.cold().with_hot_set(Arc::clone(hot_set))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn query(value: &Value) -> StructuredQuery {
    StructuredQuery::from_value(value, QueryLimits::default()).unwrap()
}

fn queries() -> Vec<StructuredQuery> {
    vec![
        query(&json!({"$ops": [{"group": {"$eq": 2}}]})),
        query(&json!({"$ops": [{"group": {"$eq": 3}}], "$limit": 4})),
        query(&json!({"$ops": [{"number": {"$gt": 40}}]})),
        query(&json!({})),
    ]
}

fn summary(rows: &[ReadDocument]) -> Vec<(String, Value)> {
    rows.iter()
        .map(|row| {
            (
                row.metadata.id.clone(),
                Value::Object(row.document.fields().clone()),
            )
        })
        .collect()
}

#[test]
fn warm_reads_match_storage_and_come_from_memory() {
    let fixture = Fixture::new();
    fixture.seed(50);
    let hot_set = Arc::new(HotSet::new(HotSet::DEFAULT_BUDGET_BYTES));
    let hot = fixture.hot(&hot_set);
    let cold = fixture.cold();
    for query in queries() {
        let expected = summary(&cold.find("records", &query).unwrap());
        assert_eq!(summary(&hot.find("records", &query).unwrap()), expected);
        assert_eq!(summary(&hot.find("records", &query).unwrap()), expected);
    }
    for number in [0, 7, 49] {
        let id = Fixture::id(number);
        assert_eq!(
            hot.get("records", &id).unwrap(),
            cold.get("records", &id).unwrap()
        );
    }
    let warm = hot_set.statistics();
    assert_eq!(warm.documents, 50);
    for query in queries() {
        hot.find("records", &query).unwrap();
    }
    let after = hot_set.statistics();
    assert_eq!(after.misses, warm.misses, "a warm pass read storage");
    assert!(after.hits > warm.hits);
    assert!(after.resident_bytes <= after.budget_bytes);
}

#[test]
fn a_cleared_hot_set_sees_the_next_write() {
    let fixture = Fixture::new();
    fixture.seed(3);
    let hot_set = Arc::new(HotSet::new(HotSet::DEFAULT_BUDGET_BYTES));
    let hot = fixture.hot(&hot_set);
    let by_group = query(&json!({"$ops": [{"group": {"$eq": 9}}]}));
    assert!(hot.find("records", &by_group).unwrap().is_empty());
    fixture.patch(1, &json!({"number": 1, "group": 9}));
    hot_set.clear();
    let rows = hot.find("records", &by_group).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].metadata.id, Fixture::id(1));
    assert_eq!(
        hot.get("records", &Fixture::id(1))
            .unwrap()
            .document
            .fields()["group"],
        json!(9)
    );
}

#[test]
fn a_write_it_was_not_told_about_is_caught_on_the_next_miss() {
    let fixture = Fixture::new();
    fixture.seed(2);
    let hot_set = Arc::new(HotSet::new(HotSet::DEFAULT_BUDGET_BYTES));
    let hot = fixture.hot(&hot_set);
    hot.get("records", &Fixture::id(0)).unwrap();
    fixture.put(5, &json!({"number": 5, "group": 0}));
    let added = hot.get("records", &Fixture::id(5)).unwrap();
    assert_eq!(added.document.fields()["number"], json!(5));
    let statistics = hot_set.statistics();
    assert_eq!(statistics.clears, 1, "the stale generation was dropped");
    let everything = hot.find("records", &query(&json!({}))).unwrap();
    assert_eq!(everything.len(), 3);
}

#[test]
fn the_budget_bounds_what_stays_resident() {
    let fixture = Fixture::new();
    fixture.seed(120);
    let budget = 24 * 1024;
    let hot_set = Arc::new(HotSet::new(budget));
    let hot = fixture.hot(&hot_set);
    let cold = fixture.cold();
    for round in 0..3 {
        for query in queries() {
            let expected = summary(&cold.find("records", &query).unwrap());
            assert_eq!(summary(&hot.find("records", &query).unwrap()), expected);
            assert!(
                hot_set.statistics().resident_bytes <= budget,
                "round {round}"
            );
        }
        for number in (0..120).step_by(7) {
            hot.get("records", &Fixture::id(number)).unwrap();
            assert!(hot_set.statistics().resident_bytes <= budget);
        }
    }
    let statistics = hot_set.statistics();
    assert!(statistics.evictions > 0);
    assert!(statistics.documents < 120);
}

#[test]
fn a_document_larger_than_the_budget_is_served_but_not_kept() {
    let fixture = Fixture::new();
    fixture.put(0, &json!({"body": "y".repeat(8 * 1024)}));
    let hot_set = Arc::new(HotSet::new(4 * 1024));
    let hot = fixture.hot(&hot_set);
    for _ in 0..2 {
        let read = hot.get("records", &Fixture::id(0)).unwrap();
        assert_eq!(
            read.document.fields()["body"].as_str().unwrap().len(),
            8 * 1024
        );
    }
    let statistics = hot_set.statistics();
    assert_eq!(statistics.documents, 0);
    assert!(statistics.refusals >= 2);
}

#[test]
fn file_collections_report_their_kind_and_read_from_storage() {
    let fixture = Fixture::new();
    fixture
        .storage()
        .create_collection("blobs", CollectionKind::File, None)
        .unwrap();
    let hot_set = Arc::new(HotSet::new(HotSet::DEFAULT_BUDGET_BYTES));
    let hot = fixture.hot(&hot_set);
    assert_eq!(hot.collection_kind("blobs").unwrap(), CollectionKind::File);
    assert_eq!(
        hot.collection_kind("records").unwrap(),
        CollectionKind::Document
    );
    let id = Fixture::id(0);
    let cold_error = fixture.cold().get("blobs", &id).unwrap_err();
    let hot_error = hot.get("blobs", &id).unwrap_err();
    assert_eq!(hot_error.code(), cold_error.code());
    assert!(hot.collection_kind("missing").is_err());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn every_read_checks_its_own_actor() {
    use std::os::unix::fs::MetadataExt;

    use fylo_engine::AccessContext;

    let fixture = Fixture::new();
    let metadata = fs::metadata(&fixture.0).unwrap();
    let private = WriteAccess {
        uid: Some(metadata.uid()),
        gid: Some(metadata.gid()),
        mode: Some(0o600),
    };
    fixture.put_with_access(0, &json!({"group": 1}), private);
    fixture.put(1, &json!({"group": 1}));
    let owner = AccessContext::new(metadata.uid(), [metadata.gid()]);
    let stranger = AccessContext::new(metadata.uid() + 1, [metadata.gid() + 1]);
    let hot_set = Arc::new(HotSet::new(HotSet::DEFAULT_BUDGET_BYTES));
    let hot = fixture.hot(&hot_set);
    let id = Fixture::id(0);
    let denied = hot.get_as("records", &id, &stranger).unwrap_err();
    assert_eq!(denied.code(), EngineErrorCode::Access);
    assert_eq!(
        hot_set.statistics().documents,
        0,
        "a denied read is not parsed"
    );
    hot.get_as("records", &id, &owner).unwrap();
    let denied = hot.get_as("records", &id, &stranger).unwrap_err();
    assert_eq!(denied.code(), EngineErrorCode::Access);
    let by_group = query(&json!({"$ops": [{"group": {"$eq": 1}}]}));
    assert_eq!(hot.find_as("records", &by_group, &owner).unwrap().len(), 2);
    let visible = hot.find_as("records", &by_group, &stranger).unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].metadata.id, Fixture::id(1));
}
