//! Range and comparison queries on an indexed field read their candidates
//! from the prefix index, then only those documents, and return exactly what
//! a scan of every document returns.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_engine::{ReadOnlyEngine, WriteEngine};
use fylo_query::{IndexLookupValue, QueryLimits, StructuredQuery, index_entries_for_document};
use fylo_storage_native::{CollectionKind, NativeWriteRoot, WriteAccess};
use serde_json::{Map, Value, json};

struct Fixture(PathBuf);

impl Fixture {
    const COLLECTION: &str = "users";

    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fylo-range-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    /// A collection written through the engine, so its index is a snapshot
    /// and a write-ahead log.
    fn written(values: &[Value]) -> Self {
        let fixture = Self::new();
        NativeWriteRoot::open(&fixture.0)
            .unwrap()
            .create_collection(Self::COLLECTION, CollectionKind::Document, None)
            .unwrap();
        for (number, value) in values.iter().enumerate() {
            fixture.put(number, value);
        }
        fixture
    }

    /// A collection whose documents and index snapshot are files written
    /// here.
    fn handmade(values: &[Value]) -> Self {
        let fixture = Self::new();
        let collection = fixture.0.join(".collections/users");
        fs::create_dir_all(collection.join("docs/4V")).unwrap();
        fs::create_dir_all(collection.join("index")).unwrap();
        let mut keys = BTreeSet::new();
        for (number, value) in values.iter().enumerate() {
            let id = Self::id(number);
            let fields = Self::fields(value);
            keys.extend(
                index_entries_for_document(&id, &fields, |_, value| {
                    Ok::<_, std::convert::Infallible>(IndexLookupValue::plain(value))
                })
                .unwrap(),
            );
            fs::write(
                collection.join(format!("docs/4V/{id}.json")),
                serde_json::to_vec(&fields).unwrap(),
            )
            .unwrap();
        }
        let mut snapshot = keys.into_iter().collect::<Vec<_>>().join("\n");
        snapshot.push('\n');
        fs::write(collection.join("index/keys.snapshot"), snapshot).unwrap();
        fixture
    }

    /// Replace the first `original` text of the index snapshot.
    fn edit_snapshot(&self, original: &str, replacement: &str) {
        let path = self.0.join(".collections/users/index/keys.snapshot");
        let snapshot = fs::read_to_string(&path).unwrap();
        assert!(snapshot.contains(original), "{original}");
        fs::write(&path, snapshot.replacen(original, replacement, 1)).unwrap();
    }

    fn id(number: usize) -> String {
        format!("4VRNF52J{number:03}")
    }

    fn fields(value: &Value) -> Map<String, Value> {
        let mut fields = json!({ "group": "all", "profile": { "score": value } });
        if !value.is_null() {
            fields["value"] = value.clone();
        }
        fields.as_object().unwrap().clone()
    }

    fn put(&self, number: usize, value: &Value) {
        WriteEngine::open(&self.0)
            .unwrap()
            .put_document(
                Self::COLLECTION,
                &Self::id(number),
                Self::fields(value),
                WriteAccess::default(),
            )
            .unwrap();
    }

    fn engine(&self) -> ReadOnlyEngine {
        ReadOnlyEngine::open(&self.0).unwrap()
    }

    fn find(&self, query: &Value) -> Vec<String> {
        let parsed = StructuredQuery::from_value(query, QueryLimits::default()).unwrap();
        self.engine()
            .find(Self::COLLECTION, &parsed)
            .unwrap()
            .into_iter()
            .map(|row| row.metadata.id)
            .collect()
    }

    /// What reading every document and testing the predicate returns.
    fn scanned(&self, query: &Value) -> Vec<String> {
        let parsed = StructuredQuery::from_value(query, QueryLimits::default()).unwrap();
        let every = StructuredQuery::from_value(&json!({}), QueryLimits::default()).unwrap();
        let mut expected: Vec<String> = self
            .engine()
            .find(Self::COLLECTION, &every)
            .unwrap()
            .into_iter()
            .filter(|row| {
                parsed.matches(
                    row.document.fields(),
                    row.metadata.created_at,
                    row.metadata.mtime,
                )
            })
            .map(|row| row.metadata.id)
            .collect();
        if let Some(limit) = parsed.limit().filter(|limit| *limit > 0) {
            expected.truncate(limit);
        }
        expected
    }

    fn assert_parity(&self, query: &Value) {
        assert_eq!(self.find(query), self.scanned(query), "{query}");
    }

    fn document_path(&self, number: usize) -> PathBuf {
        Self::find_file(&self.0, &format!("{}.json", Self::id(number))).unwrap()
    }

    fn find_file(directory: &Path, name: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if let Some(found) = Self::find_file(&path, name) {
                    return Some(found);
                }
            } else if path.file_name().is_some_and(|file| file == name) {
                return Some(path);
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

fn coercion_values() -> Vec<Value> {
    vec![
        json!(null),
        json!(false),
        json!(true),
        json!(0),
        json!(-0.0),
        json!(1),
        json!(2),
        json!(2.5),
        json!(-3),
        json!(1e300),
        json!(""),
        json!(" "),
        json!("01"),
        json!("1e1"),
        json!("0x10"),
        json!("-4"),
        json!("abc"),
        json!([]),
        json!([1]),
        json!([-2]),
        json!([1, 5]),
        json!(["7"]),
        json!({ "nested": 1 }),
    ]
}

fn range_queries() -> Vec<Value> {
    let mut queries = Vec::new();
    for operand in [
        json!({ "$gt": 0 }),
        json!({ "$gte": 0 }),
        json!({ "$lt": 0 }),
        json!({ "$lte": 0 }),
        json!({ "$gt": -0.0 }),
        json!({ "$gt": 1 }),
        json!({ "$gte": 1 }),
        json!({ "$gte": 1, "$lt": 3 }),
        json!({ "$gt": 1, "$lte": 2.5 }),
        json!({ "$lt": -1 }),
        json!({ "$lte": -3 }),
        json!({ "$gt": -1, "$lt": 1 }),
        json!({ "$gte": 5, "$lt": 1 }),
        json!({ "$gt": "1" }),
        json!({ "$gte": true }),
        json!({ "$gte": [2] }),
        json!({ "$gte": "abc" }),
        json!({ "$gt": 1, "$ne": 2 }),
        json!({ "$gte": 1, "$eq": 2 }),
        json!({ "$gte": 1e299 }),
    ] {
        queries.push(json!({ "$ops": [{ "value": operand }] }));
    }
    queries
}

#[test]
fn range_queries_match_a_full_scan_for_coercions_arrays_and_paths() {
    let fixture = Fixture::written(&coercion_values());
    for query in range_queries() {
        fixture.assert_parity(&query);
    }
    for path in ["profile.score", "profile/score", "/profile//score."] {
        fixture.assert_parity(&json!({ "$ops": [{ path: { "$gte": 1 } }] }));
    }
}

#[test]
fn range_queries_combine_with_other_operations_and_limits() {
    let fixture = Fixture::written(&coercion_values());
    for query in [
        json!({ "$ops": [{ "group": { "$eq": "all" }, "value": { "$gt": 1 } }] }),
        json!({ "$ops": [{ "value": { "$gt": 1 } }, { "value": { "$lt": -1 } }] }),
        json!({ "$ops": [{ "value": { "$gt": 1 } }, { "value": { "$eq": "abc" } }] }),
        json!({ "$ops": [{ "value": { "$gt": 1 } }, { "value": { "$gte": 0 } }] }),
        json!({ "$ops": [{ "value": { "$gt": 1 }, "profile.score": { "$lt": 2 } }] }),
        json!({ "$ops": [{ "value": { "$gt": 0 } }], "$limit": 3 }),
        json!({ "$ops": [{ "value": { "$gt": 0 } }], "$limit": 0 }),
    ] {
        fixture.assert_parity(&query);
    }
}

#[test]
fn range_queries_follow_puts_patches_and_deletes() {
    let values: Vec<Value> = (0..30).map(|number| json!(number)).collect();
    let fixture = Fixture::written(&values);
    let query = json!({ "$ops": [{ "value": { "$gte": 10, "$lt": 20 } }] });
    fixture.assert_parity(&query);
    let writer = NativeWriteRoot::open(&fixture.0).unwrap();
    writer
        .patch_document("users", &Fixture::id(3), br#"{"value":"15"}"#, None)
        .unwrap();
    writer
        .patch_document("users", &Fixture::id(12), br#"{"value":[]}"#, None)
        .unwrap();
    writer
        .delete_document("users", &Fixture::id(14), None)
        .unwrap();
    fixture.put(40, &json!(19.5));
    fixture.assert_parity(&query);
    assert_eq!(fixture.find(&query).len(), 10);
    writer
        .restore_document("users", &Fixture::id(14), None)
        .unwrap();
    fixture.assert_parity(&query);
    assert_eq!(fixture.find(&query).len(), 11);
}

#[test]
fn a_range_query_reads_only_the_documents_in_its_range() {
    let values: Vec<Value> = (0..20).map(|number| json!(number)).collect();
    let fixture = Fixture::written(&values);
    fs::write(fixture.document_path(2), b"not a document").unwrap();
    let every = json!({});
    let every = StructuredQuery::from_value(&every, QueryLimits::default()).unwrap();
    assert!(fixture.engine().find("users", &every).is_err());
    let expected: Vec<String> = (15..20).map(Fixture::id).collect();
    assert_eq!(
        fixture.find(&json!({ "$ops": [{ "value": { "$gte": 15 } }] })),
        expected
    );
    assert_eq!(
        fixture.find(&json!({ "$ops": [{ "value": { "$gt": 14.5, "$lte": 19 } }] })),
        expected
    );
}

#[test]
fn an_indexed_query_reads_only_its_part_of_the_index() {
    let values: Vec<Value> = (0..20).map(|number| json!(number)).collect();
    let fixture = Fixture::handmade(&values);
    let first_key = "group/eq/all/4VRNF52J000\n";
    fixture.edit_snapshot(first_key, &format!("{first_key}group/eq/aaa/4VRNF52J001\n"));
    assert!(fixture.engine().verify_index("users").is_err());
    assert_eq!(
        fixture.find(&json!({ "$ops": [{ "value": { "$eq": 7 } }] })),
        [Fixture::id(7)]
    );
    assert_eq!(
        fixture.find(&json!({ "$ops": [{ "value": { "$lt": 2 } }] })),
        [Fixture::id(0), Fixture::id(1)]
    );
}

#[test]
fn a_damaged_run_of_keys_the_query_reads_is_reported() {
    let values: Vec<Value> = (0..20).map(|number| json!(number)).collect();
    let fixture = Fixture::handmade(&values);
    let key = "value/eq/9/4VRNF52J009\n";
    fixture.edit_snapshot(key, &format!("{key}value/eq/9/4VRNF52J001\n"));
    let query = json!({ "$ops": [{ "value": { "$eq": 9 } }] });
    let parsed = StructuredQuery::from_value(&query, QueryLimits::default()).unwrap();
    assert!(fixture.engine().find("users", &parsed).is_err());
}
