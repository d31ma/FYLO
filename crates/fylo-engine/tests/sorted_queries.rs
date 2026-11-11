//! `$sort` walks the sort field's numeric keys in order and reads only the
//! documents it returns; `$after` resumes from a stateless cursor (ADR 0023
//! sections 5 and 6).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_engine::{ReadOnlyEngine, WriteEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{CollectionKind, CollectionLayout, NativeWriteRoot};
use serde_json::{Value, json};

struct Fixture(PathBuf);

/// One stored document: its sort value as stored, and the number it sorts
/// by, or `None` when it has none.
struct Row {
    value: Value,
    sorts_by: Option<f64>,
    conversation: &'static str,
}

impl Fixture {
    const COLLECTION: &str = "messages";

    fn new(layout: CollectionLayout) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fylo-sorted-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        NativeWriteRoot::open(&root)
            .unwrap()
            .create_collection_with_layout(
                Self::COLLECTION,
                CollectionKind::Document,
                None,
                Some(layout),
            )
            .unwrap();
        let fixture = Self(root);
        for (number, row) in rows().iter().enumerate() {
            fixture.put(number, row.conversation, &row.value);
        }
        fixture
    }

    fn id(number: usize) -> String {
        format!("4VRNF52J{number:03}")
    }

    fn put(&self, number: usize, conversation: &str, value: &Value) {
        let mut fields = json!({ "conversation": conversation, "body": format!("note {number}") });
        if !value.is_null() || number % 2 == 0 {
            fields["sentAt"] = value.clone();
        }
        WriteEngine::open(&self.0)
            .unwrap()
            .put_document(
                Self::COLLECTION,
                &Self::id(number),
                fields.as_object().unwrap().clone(),
            )
            .unwrap();
    }

    fn engine(&self) -> ReadOnlyEngine {
        ReadOnlyEngine::open(&self.0).unwrap()
    }

    fn page(&self, query: &Value) -> (Vec<String>, Option<String>) {
        let parsed = StructuredQuery::from_value(query, QueryLimits::default()).unwrap();
        let page = self
            .engine()
            .find_sorted(Self::COLLECTION, &parsed)
            .unwrap();
        let ids = page.rows.into_iter().map(|row| row.metadata.id).collect();
        (ids, page.next_cursor)
    }

    /// Every page of `query`, `limit` at a time, joined.
    fn all_pages(&self, query: &Value, limit: usize) -> Vec<String> {
        let mut ids = Vec::new();
        let mut query = query.clone();
        query["$limit"] = json!(limit);
        loop {
            let (page, next) = self.page(&query);
            assert!(page.len() <= limit);
            ids.extend(page);
            let Some(next) = next else {
                return ids;
            };
            query["$after"] = json!(next);
        }
    }

    fn damage(&self, number: usize) {
        let name = format!("{}.json", Self::id(number));
        let path = Self::find_file(&self.0.join(".collections"), &name).unwrap();
        fs::write(path, b"not a document").unwrap();
    }

    fn find_file(directory: &Path, name: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(directory).ok()? {
            let path = entry.ok()?.path();
            if path.file_name().is_some_and(|file| file == name) {
                return Some(path);
            }
            if path.is_dir()
                && let Some(found) = Self::find_file(&path, name)
            {
                return Some(found);
            }
        }
        None
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn row(value: Value, sorts_by: Option<f64>, conversation: &'static str) -> Row {
    Row {
        value,
        sorts_by,
        conversation,
    }
}

fn rows() -> Vec<Row> {
    vec![
        row(
            json!(1_700_000_000_300_i64),
            Some(1_700_000_000_300.0),
            "c-1",
        ),
        row(
            json!(1_700_000_000_100_i64),
            Some(1_700_000_000_100.0),
            "c-2",
        ),
        row(json!(-5), Some(-5.0), "c-1"),
        row(json!("15"), Some(15.0), "c-2"),
        row(json!("not a number"), None, "c-1"),
        row(json!(true), Some(1.0), "c-2"),
        row(json!(null), Some(0.0), "c-1"),
        row(json!(null), None, "c-2"),
        row(json!([40, 2, 99]), Some(f64::NAN), "c-1"),
        row(json!([]), None, "c-2"),
        row(json!(15), Some(15.0), "c-1"),
        row(json!(2.5), Some(2.5), "c-2"),
        row(json!(15), Some(15.0), "c-2"),
        row(json!({ "nested": 4 }), None, "c-1"),
        row(json!(-0.5), Some(-0.5), "c-2"),
    ]
}

/// What a scan returns for `$sort` on `sentAt`, with `keep` as the filter.
fn expected(descending: bool, keep: impl Fn(&Row) -> bool) -> Vec<String> {
    let mut keyed: Vec<(f64, String)> = rows()
        .iter()
        .enumerate()
        .filter(|(_, row)| keep(row))
        .filter_map(|(number, row)| {
            let value = match row.sorts_by {
                Some(value) if value.is_nan() => {
                    if descending {
                        99.0
                    } else {
                        2.0
                    }
                }
                other => other?,
            };
            Some((value, Fixture::id(number)))
        })
        .collect();
    keyed.sort_by(|left, right| {
        let order = left.0.partial_cmp(&right.0).unwrap();
        let order = if descending { order.reverse() } else { order };
        order.then_with(|| left.1.cmp(&right.1))
    });
    keyed.into_iter().map(|(_, id)| id).collect()
}

#[test]
fn sorted_pages_equal_a_sorted_scan_in_both_directions() {
    for layout in [CollectionLayout::Files, CollectionLayout::Segments] {
        let fixture = Fixture::new(layout);
        let ascending = json!({ "$sort": { "sentAt": "asc" } });
        let descending = json!({ "$sort": { "sentAt": "desc" } });
        assert_eq!(fixture.page(&ascending).0, expected(false, |_| true));
        assert_eq!(fixture.page(&descending).0, expected(true, |_| true));
        assert_eq!(fixture.page(&ascending).1, None);
        for limit in [1, 2, 3, 5, 100] {
            assert_eq!(
                fixture.all_pages(&ascending, limit),
                expected(false, |_| true)
            );
            assert_eq!(
                fixture.all_pages(&descending, limit),
                expected(true, |_| true)
            );
        }
    }
}

#[test]
fn a_filter_and_a_sort_combine() {
    for layout in [CollectionLayout::Files, CollectionLayout::Segments] {
        let fixture = Fixture::new(layout);
        let query = json!({
            "$ops": [{ "conversation": { "$eq": "c-2" } }],
            "$sort": { "sentAt": "desc" }
        });
        let wanted = expected(true, |row| row.conversation == "c-2");
        assert_eq!(fixture.page(&query).0, wanted);
        assert_eq!(fixture.all_pages(&query, 2), wanted);
        let unplanned = json!({
            "$ops": [{ "body": { "$like": "note%" }, "conversation": { "$eq": "c-1" } }],
            "$sort": { "sentAt": "asc" }
        });
        assert_eq!(
            fixture.all_pages(&unplanned, 3),
            expected(false, |row| row.conversation == "c-1")
        );
    }
}

#[test]
fn a_sorted_page_reads_only_the_documents_it_returns() {
    let fixture = Fixture::new(CollectionLayout::Files);
    let order = expected(true, |_| true);
    let damaged = order[3].clone();
    fixture.damage(damaged[8..].parse().unwrap());
    let (page, next) = fixture.page(&json!({ "$sort": { "sentAt": "desc" }, "$limit": 2 }));
    assert_eq!(page, order[..2]);
    assert!(next.is_some());
    let every = StructuredQuery::from_value(&json!({}), QueryLimits::default()).unwrap();
    assert!(fixture.engine().find("messages", &every).is_err());
}

#[test]
fn a_cursor_resumes_after_writes_between_pages() {
    let fixture = Fixture::new(CollectionLayout::Files);
    let query = json!({ "$sort": { "sentAt": "desc" }, "$limit": 3 });
    let (first, next) = fixture.page(&query);
    assert_eq!(first, expected(true, |_| true)[..3]);
    fixture.put(50, "c-1", &json!(2_000_000_000_000_i64));
    fixture.put(51, "c-1", &json!(3));
    let mut second = query.clone();
    second["$after"] = json!(next.unwrap());
    let (page, _) = fixture.page(&second);
    assert!(!page.contains(&Fixture::id(50)));
    assert!(!page.iter().any(|id| first.contains(id)));
    let remaining = fixture.all_pages(&second, 3);
    assert!(remaining.contains(&Fixture::id(51)));
}

#[test]
fn a_cursor_from_another_query_or_garbage_is_refused() {
    let fixture = Fixture::new(CollectionLayout::Files);
    let (_, next) = fixture.page(&json!({ "$sort": { "sentAt": "desc" }, "$limit": 2 }));
    let next = next.unwrap();
    for query in [
        json!({ "$sort": { "sentAt": "asc" }, "$after": next }),
        json!({ "$sort": { "body": "desc" }, "$after": next }),
        json!({ "$ops": [{ "conversation": { "$eq": "c-1" } }], "$sort": { "sentAt": "desc" }, "$after": next }),
        json!({ "$after": next }),
        json!({ "$sort": { "sentAt": "desc" }, "$after": "fys1.d.nothex.4V.0000000000000000" }),
        json!({ "$sort": { "sentAt": "desc" }, "$after": "garbage" }),
        json!({ "$sort": { "sentAt": "desc" }, "$after": 7 }),
    ] {
        let error = StructuredQuery::from_value(&query, QueryLimits::default()).unwrap_err();
        assert_eq!(error.code().as_str(), "EINVALIDCURSOR", "{query}");
    }
    let mut same = json!({ "$sort": { "sentAt": "desc" }, "$limit": 5 });
    same["$after"] = json!(next);
    assert!(StructuredQuery::from_value(&same, QueryLimits::default()).is_ok());
}

#[test]
fn a_malformed_sort_is_refused() {
    for sort in [
        json!("sentAt"),
        json!({}),
        json!({ "sentAt": "up" }),
        json!({ "sentAt": "asc", "body": "desc" }),
    ] {
        let query = json!({ "$sort": sort });
        let error = StructuredQuery::from_value(&query, QueryLimits::default()).unwrap_err();
        assert_eq!(error.code().as_str(), "EQUERY_INVALID", "{query}");
    }
}

#[test]
fn find_returns_sorted_rows_when_the_query_has_a_sort() {
    let fixture = Fixture::new(CollectionLayout::Segments);
    let query = StructuredQuery::from_value(
        &json!({ "$sort": { "sentAt": "asc" }, "$limit": 4 }),
        QueryLimits::default(),
    )
    .unwrap();
    let ids: Vec<String> = fixture
        .engine()
        .find("messages", &query)
        .unwrap()
        .into_iter()
        .map(|row| row.metadata.id)
        .collect();
    assert_eq!(ids, expected(false, |_| true)[..4]);
}
