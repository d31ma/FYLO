//! Query results in the hot set (ADR 0030 section 3.5): a repeated query is
//! answered from its kept match list exactly as storage answers it, for any
//! limit; a query cut short by its limit is not kept; and a write, after the
//! clear a write frame does, is always seen.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use byckr_engine::{HotSet, ReadDocument, ReadOnlyEngine, WriteEngine};
use byckr_query::{QueryLimits, StructuredQuery};
use byckr_storage_native::{CollectionKind, NativeWriteRoot};
use serde_json::{Map, Value, json};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = byckr_test_support::TestFolder::path().join(format!(
            "fylo-hot-set-queries-{}-{}",
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
        self.writer()
            .put_document("records", &Self::id(number), Self::fields(value))
            .unwrap();
    }

    fn patch(&self, number: usize, value: &Value) {
        self.writer()
            .patch_document("records", &Self::id(number), Self::fields(value))
            .unwrap();
    }

    fn seed(&self, count: usize) {
        for number in 0..count {
            self.put(number, &json!({"number": number, "group": number % 5}));
        }
    }

    fn cold(&self) -> ReadOnlyEngine {
        ReadOnlyEngine::open(&self.0).unwrap()
    }

    fn hot(&self, hot_set: &Arc<HotSet>) -> ReadOnlyEngine {
        self.cold().with_hot_set(Arc::clone(hot_set))
    }

    fn expected(&self, query: &StructuredQuery) -> Vec<(String, Value)> {
        summary(&self.cold().find("records", query).unwrap())
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

fn group(value: usize) -> StructuredQuery {
    query(&json!({"$ops": [{"group": {"$eq": value}}]}))
}

fn group_limited(value: usize, limit: usize) -> StructuredQuery {
    query(&json!({"$ops": [{"group": {"$eq": value}}], "$limit": limit}))
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

fn shared_hot_set() -> Arc<HotSet> {
    Arc::new(HotSet::new(HotSet::DEFAULT_BUDGET_BYTES))
}

#[test]
fn a_repeated_query_is_answered_from_its_match_list() {
    let fixture = Fixture::new();
    fixture.seed(50);
    let hot_set = shared_hot_set();
    let hot = fixture.hot(&hot_set);
    let by_group = group(2);
    let first = summary(&hot.find("records", &by_group).unwrap());
    let kept = hot_set.statistics();
    let second = summary(&hot.find("records", &by_group).unwrap());
    let after = hot_set.statistics();
    assert_eq!(first, fixture.expected(&by_group));
    assert_eq!(second, first);
    assert_eq!(kept.queries, 1);
    assert_eq!(kept.query_misses, 1);
    assert_eq!(after.query_hits, kept.query_hits + 1);
    assert_eq!(after.misses, kept.misses, "the repeat read storage");
}

#[test]
fn every_limit_of_a_kept_query_matches_storage() {
    let fixture = Fixture::new();
    fixture.seed(50);
    let hot_set = shared_hot_set();
    let hot = fixture.hot(&hot_set);
    hot.find("records", &group(3)).unwrap();
    for limit in [1, 3, 10, 11, 100, 0] {
        let limited = group_limited(3, limit);
        assert_eq!(
            summary(&hot.find("records", &limited).unwrap()),
            fixture.expected(&limited),
            "limit {limit}"
        );
    }
    let statistics = hot_set.statistics();
    assert_eq!(statistics.queries, 1);
    assert_eq!(statistics.query_hits, 6);
}

#[test]
fn a_query_cut_short_by_its_limit_is_not_kept() {
    let fixture = Fixture::new();
    fixture.seed(50);
    let hot_set = shared_hot_set();
    let hot = fixture.hot(&hot_set);
    let cut = group_limited(4, 4);
    assert_eq!(
        summary(&hot.find("records", &cut).unwrap()),
        fixture.expected(&cut)
    );
    assert_eq!(hot_set.statistics().queries, 0);
}

#[test]
fn a_query_with_a_limit_above_its_matches_is_kept() {
    let fixture = Fixture::new();
    fixture.seed(50);
    let hot_set = shared_hot_set();
    let hot = fixture.hot(&hot_set);
    hot.find("records", &group_limited(4, 100)).unwrap();
    assert_eq!(hot_set.statistics().queries, 1);
    assert_eq!(
        summary(&hot.find("records", &group(4)).unwrap()),
        fixture.expected(&group(4))
    );
}

#[test]
fn a_changed_document_is_seen_after_a_write_clears_the_hot_set() {
    let fixture = Fixture::new();
    fixture.seed(10);
    let hot_set = shared_hot_set();
    let hot = fixture.hot(&hot_set);
    hot.find("records", &group(9)).unwrap();
    fixture.patch(1, &json!({"number": 1, "group": 9}));
    hot_set.clear();
    let rows = hot.find("records", &group(9)).unwrap();
    assert_eq!(summary(&rows), fixture.expected(&group(9)));
    assert_eq!(rows.len(), 1);
}

#[test]
fn a_deleted_document_is_gone_after_a_write_clears_the_hot_set() {
    let fixture = Fixture::new();
    fixture.seed(10);
    let hot_set = shared_hot_set();
    let hot = fixture.hot(&hot_set);
    let before = hot.find("records", &group(1)).unwrap();
    fixture
        .storage()
        .delete_document("records", &Fixture::id(1))
        .unwrap();
    hot_set.clear();
    let after = hot.find("records", &group(1)).unwrap();
    assert_eq!(summary(&after), fixture.expected(&group(1)));
    assert_eq!(after.len(), before.len() - 1);
}

#[test]
fn an_inserted_document_is_found_after_a_write_clears_the_hot_set() {
    let fixture = Fixture::new();
    fixture.seed(10);
    let hot_set = shared_hot_set();
    let hot = fixture.hot(&hot_set);
    let before = hot.find("records", &group(2)).unwrap();
    fixture.put(42, &json!({"number": 42, "group": 2}));
    hot_set.clear();
    let after = hot.find("records", &group(2)).unwrap();
    assert_eq!(summary(&after), fixture.expected(&group(2)));
    assert_eq!(after.len(), before.len() + 1);
}

#[test]
fn a_collection_created_again_is_read_afresh_after_a_clear() {
    let fixture = Fixture::new();
    fixture.seed(10);
    let hot_set = shared_hot_set();
    let hot = fixture.hot(&hot_set);
    hot.find("records", &group(0)).unwrap();
    let storage = fixture.storage();
    storage.drop_collection("records").unwrap();
    storage
        .create_collection("records", CollectionKind::Document, None)
        .unwrap();
    fixture.put(3, &json!({"number": 3, "group": 0}));
    hot_set.clear();
    let rows = hot.find("records", &group(0)).unwrap();
    assert_eq!(summary(&rows), fixture.expected(&group(0)));
    assert_eq!(rows.len(), 1);
}

#[test]
fn the_budget_bounds_kept_match_lists() {
    let fixture = Fixture::new();
    fixture.seed(50);
    let hot_set = Arc::new(HotSet::new(4 * 1024));
    let hot = fixture.hot(&hot_set);
    for value in 0..5 {
        assert_eq!(
            summary(&hot.find("records", &group(value)).unwrap()),
            fixture.expected(&group(value))
        );
    }
    let statistics = hot_set.statistics();
    assert!(statistics.resident_bytes <= statistics.budget_bytes);
}

#[test]
fn a_zero_budget_keeps_no_match_list() {
    let fixture = Fixture::new();
    fixture.seed(10);
    let hot_set = Arc::new(HotSet::new(0));
    let hot = fixture.hot(&hot_set);
    assert_eq!(
        summary(&hot.find("records", &group(1)).unwrap()),
        fixture.expected(&group(1))
    );
    assert_eq!(hot_set.statistics().queries, 0);
}

#[test]
fn a_match_list_larger_than_the_budget_is_answered_but_not_kept() {
    let fixture = Fixture::new();
    fixture.seed(50);
    let hot_set = Arc::new(HotSet::new(1500));
    let hot = fixture.hot(&hot_set);
    let everything = query(&json!({}));
    assert_eq!(
        summary(&hot.find("records", &everything).unwrap()),
        fixture.expected(&everything)
    );
    let statistics = hot_set.statistics();
    assert_eq!(statistics.queries, 0);
    assert!(statistics.refusals > 0);
    assert!(statistics.resident_bytes <= statistics.budget_bytes);
}
