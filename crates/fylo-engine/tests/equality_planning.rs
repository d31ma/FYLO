use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_engine::ReadOnlyEngine;
use fylo_query::{IndexLookupValue, QueryLimits, StructuredQuery, index_entries_for_document};
use serde_json::{Value, json};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fylo-equality-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self(root);
        fs::create_dir_all(fixture.0.join(".collections/users/docs/4V")).unwrap();
        fs::create_dir_all(fixture.0.join(".collections/users/index")).unwrap();
        let mut keys = BTreeSet::new();
        let values = [
            json!(null),
            json!(false),
            json!(true),
            json!(0),
            json!(-0.0),
            json!(1),
            json!(2),
            json!(""),
            json!(" "),
            json!("01"),
            json!("1"),
            json!("+1"),
            json!("1e0"),
            json!("0x1"),
            json!("1.0"),
            json!("-0"),
            json!("true"),
            json!("other"),
            json!([1]),
            json!({"nested":1}),
        ];
        for (number, value) in values.into_iter().enumerate() {
            let id = format!("4VRNF52J{number:03}");
            let mut fields = json!({"value":value,"group":"all","profile":{"score":value}});
            if number == 0 {
                fields.as_object_mut().unwrap().remove("value");
            }
            let fields = fields.as_object().unwrap();
            keys.extend(
                index_entries_for_document(&id, fields, |_, value| {
                    Ok::<_, std::convert::Infallible>(IndexLookupValue::plain(value))
                })
                .unwrap(),
            );
            fs::write(
                fixture
                    .0
                    .join(format!(".collections/users/docs/4V/{id}.json")),
                serde_json::to_vec(fields).unwrap(),
            )
            .unwrap();
        }
        fs::write(
            fixture.0.join(".collections/users/index/keys.snapshot"),
            keys.into_iter().collect::<Vec<_>>().join("\n"),
        )
        .unwrap();
        fixture
    }

    fn assert_parity(&self, query: &Value) {
        let engine = ReadOnlyEngine::open(&self.0).unwrap();
        let all = StructuredQuery::from_value(&json!({}), QueryLimits::default()).unwrap();
        let parsed = StructuredQuery::from_value(query, QueryLimits::default()).unwrap();
        let mut expected = engine
            .find("users", &all)
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
            .collect::<Vec<_>>();
        if let Some(limit) = parsed.limit().filter(|limit| *limit > 0) {
            expected.truncate(limit);
        }
        let actual = engine
            .find("users", &parsed)
            .unwrap()
            .into_iter()
            .map(|row| row.metadata.id)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{query}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn equality_matches_full_scan_for_coercions_and_missing_values() {
    let fixture = Fixture::new();
    for value in [
        json!(1),
        json!(true),
        json!(0),
        json!(-0.0),
        json!(false),
        json!(""),
        json!("01"),
        json!("1"),
        json!("true"),
        json!(null),
        json!([1]),
    ] {
        fixture.assert_parity(&json!({"$ops":[{"value":{"$eq":value}}]}));
    }
}

#[test]
fn partial_conjunctions_unbounded_or_and_limits_preserve_results() {
    let fixture = Fixture::new();
    for query in [
        json!({"$ops":[{"group":{"$eq":"all"},"value":{"$eq":null}}]}),
        json!({"$ops":[{"value":{"$eq":1,"$ne":true}}]}),
        json!({"$ops":[{"value":{"$eq":1}},{"value":{"$eq":null}}]}),
        json!({"$ops":[{"value":{"$eq":1}},{"value":{"$ne":0}}]}),
        json!({"$ops":[{"value":{"$eq":1}}],"$limit":2}),
        json!({"$ops":[{"value":{"$eq":1}}],"$limit":0}),
    ] {
        fixture.assert_parity(&query);
    }
}

#[test]
fn equality_uses_the_same_nested_path_normalization_as_predicates() {
    let fixture = Fixture::new();
    for path in [
        "profile.score",
        "profile/score",
        ".profile..score/",
        "/profile//score.",
    ] {
        fixture.assert_parity(&json!({"$ops":[{path:{"$eq":1}}]}));
    }
}

#[test]
fn sql_equality_includes_coercion_matches() {
    let fixture = Fixture::new();
    let engine = ReadOnlyEngine::open(&fixture.0).unwrap();
    let plan = fylo_query::prepare_sql(
        "SELECT * FROM users WHERE value = 1",
        QueryLimits::default(),
    )
    .unwrap();
    let query = StructuredQuery::from_value(&plan.ast, QueryLimits::default()).unwrap();
    let all = StructuredQuery::from_value(&json!({}), QueryLimits::default()).unwrap();
    let expected = engine
        .find("users", &all)
        .unwrap()
        .into_iter()
        .filter(|row| {
            query.matches(
                row.document.fields(),
                row.metadata.created_at,
                row.metadata.mtime,
            )
        })
        .count();
    assert_eq!(
        engine.select_sql(&plan).unwrap().as_object().unwrap().len(),
        expected
    );
}

#[cfg(unix)]
#[test]
fn repeated_planned_reads_recheck_revoked_access() {
    use fylo_engine::AccessContext;
    let fixture = Fixture::new();
    let engine = ReadOnlyEngine::open(&fixture.0).unwrap();
    let query = StructuredQuery::from_value(
        &json!({"$ops":[{"value":{"$eq":1}}]}),
        QueryLimits::default(),
    )
    .unwrap();
    let actor = AccessContext::new(1000, []);
    let id = "4VRNF52J005";
    let path = fixture
        .0
        .join(format!(".collections/users/docs/4V/{id}.json"));
    assert!(
        engine
            .find_as("users", &query, &actor)
            .unwrap()
            .iter()
            .any(|row| row.metadata.id == id)
    );
    xattr::set(
        &path,
        "user.fylo.access",
        br#"{"version":1,"uid":2000,"gid":2000,"mode":256}"#,
    )
    .unwrap();
    assert!(
        !engine
            .find_as("users", &query, &actor)
            .unwrap()
            .iter()
            .any(|row| row.metadata.id == id)
    );
}

#[test]
fn repeated_reads_follow_native_put_patch_delete_and_restore() {
    use fylo_storage_native::{NativeWriteRoot, PutDocumentOptions};
    let fixture = Fixture::new();
    let engine = ReadOnlyEngine::open(&fixture.0).unwrap();
    let writer = NativeWriteRoot::open(&fixture.0).unwrap();
    let query = StructuredQuery::from_value(
        &json!({"$ops":[{"value":{"$eq":1}}]}),
        QueryLimits::default(),
    )
    .unwrap();
    let count = || engine.find("users", &query).unwrap().len();
    let original = count();
    let id = "4VRNF52J030";
    writer
        .put_document(
            "users",
            id,
            br#"{"value":"0001"}"#,
            PutDocumentOptions::default(),
        )
        .unwrap();
    assert_eq!(count(), original + 1);
    writer
        .patch_document("users", id, br#"{"value":2}"#, None)
        .unwrap();
    assert_eq!(count(), original);
    writer
        .delete_document("users", "4VRNF52J005", None)
        .unwrap();
    assert_eq!(count(), original - 1);
    writer
        .restore_document("users", "4VRNF52J005", None)
        .unwrap();
    assert_eq!(count(), original);
}

#[test]
fn raw_file_metadata_equality_preserves_coercion_and_missing_values() {
    use fylo_storage_native::{CollectionKind, NativeWriteRoot, PutRawFileOptions};
    let fixture = Fixture::new();
    let writer = NativeWriteRoot::open(&fixture.0).unwrap();
    writer
        .create_collection("assets", CollectionKind::File, None)
        .unwrap();
    for (id, metadata) in [
        (
            "4VRNF52J031",
            std::collections::BTreeMap::from([("score".into(), json!("01"))]),
        ),
        ("4VRNF52J032", std::collections::BTreeMap::new()),
    ] {
        writer
            .put_raw_file(
                "assets",
                id,
                b"content",
                &PutRawFileOptions {
                    key: format!("/{id}.txt"),
                    extension: ".txt".into(),
                    metadata,
                    ..PutRawFileOptions::default()
                },
            )
            .unwrap();
    }
    let engine = ReadOnlyEngine::open(&fixture.0).unwrap();
    for (value, expected) in [(json!(1), "4VRNF52J031"), (json!(null), "4VRNF52J032")] {
        let query = StructuredQuery::from_value(
            &json!({"$ops":[{"meta.score":{"$eq":value}}]}),
            QueryLimits::default(),
        )
        .unwrap();
        let rows = engine.find_files("assets", &query, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, expected);
    }
}
