//! The Store query plan (`api/store/v1`, slice SDK-2): find, scroll, count,
//! exists and delete over one scan, with the Store's strict rules, its sort
//! order and pages with no bound on the offset.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_engine::{ReadOnlyEngine, StoreOverlay, WriteEngine};
use fylo_query::{StorePage, StoreQuery, StoreScrollPage};
use fylo_storage_native::{
    CollectionKind, CollectionLayout, DocumentWrite, NativeStorageErrorCode, NativeWriteRoot,
};
use serde_json::{Map, Value, json};

struct Fixture(PathBuf);

fn no_overlay() -> StoreOverlay {
    StoreOverlay::default()
}

impl Fixture {
    const COLLECTION: &str = "tickets";

    fn new(layout: CollectionLayout) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = fylo_test_support::TestFolder::path().join(format!(
            "fylo-store-{}-{}",
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
        Self(root)
    }

    fn id(number: usize) -> String {
        format!("4VRNF5{number:05}")
    }

    fn writer(&self) -> WriteEngine {
        WriteEngine::open(&self.0).unwrap()
    }

    fn reader(&self) -> ReadOnlyEngine {
        ReadOnlyEngine::open(&self.0).unwrap()
    }

    fn put(&self, number: usize, fields: &Value) {
        self.writer()
            .put_document(
                Self::COLLECTION,
                &Self::id(number),
                fields.as_object().unwrap().clone(),
            )
            .unwrap();
    }

    fn put_many(&self, first: usize, rows: &[Value]) {
        let writes: Vec<DocumentWrite> = rows
            .iter()
            .enumerate()
            .map(|(offset, fields)| DocumentWrite::Put {
                identifier: Self::id(first + offset),
                fields: fields.as_object().unwrap().clone(),
            })
            .collect();
        self.writer()
            .commit_document_writes(Self::COLLECTION, &writes)
            .unwrap();
    }

    fn put_in_batches(&self, rows: &[Value]) {
        let batch = 50;
        for (number, chunk) in rows.chunks(batch).enumerate() {
            self.put_many(number * batch, chunk);
        }
    }

    fn seed_tickets(&self) {
        let rows = [
            json!({ "status": "open", "priority": 3, "title": "alpha" }),
            json!({ "status": "open", "priority": 1, "title": "bravo" }),
            json!({ "status": "closed", "priority": 2, "title": "charlie" }),
            json!({ "status": "open", "priority": 3.0, "title": "delta" }),
            json!({ "status": "Open", "priority": "3", "title": "echo" }),
            json!({ "status": null, "priority": true, "title": "foxtrot" }),
            json!({ "priority": 5, "title": "golf" }),
            json!({ "status": "open", "priority": 2, "title": "hotel", "tags": ["a"] }),
        ];
        for (number, row) in rows.iter().enumerate() {
            self.put(number, row);
        }
    }

    fn query(value: &Value) -> StoreQuery {
        StoreQuery::from_value(Some(value), StoreQuery::DEFAULT_ID_FIELD).unwrap()
    }

    fn titles(&self, query: &Value, page: StorePage) -> Vec<String> {
        self.reader()
            .store_find(Self::COLLECTION, &Self::query(query), page, &no_overlay())
            .unwrap()
            .into_iter()
            .map(|row| row.document.fields()["title"].as_str().unwrap().to_owned())
            .collect()
    }

    fn count(&self, query: &Value) -> u64 {
        self.reader()
            .store_count(Self::COLLECTION, &Self::query(query), &no_overlay())
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn first_page() -> StorePage {
    StorePage::new(100, 0)
}

fn check_strict_equality(layout: CollectionLayout) {
    let fixture = Fixture::new(layout);
    fixture.seed_tickets();
    let open = json!({ "where": [{ "field": "status", "operator": "eq", "value": "open" }] });
    assert_eq!(
        fixture.titles(&open, first_page()),
        ["alpha", "bravo", "delta", "hotel"]
    );
    let three = json!({ "where": [{ "field": "priority", "operator": "eq", "value": 3 }] });
    assert_eq!(fixture.titles(&three, first_page()), ["alpha", "delta"]);
}

#[test]
fn equality_is_strict_by_type_and_exact_for_strings_in_files() {
    check_strict_equality(CollectionLayout::Files);
}

#[test]
fn equality_is_strict_by_type_and_exact_for_strings_in_segments() {
    check_strict_equality(CollectionLayout::Segments);
}

#[test]
fn null_matches_missing_and_null_fields() {
    let fixture = Fixture::new(CollectionLayout::Files);
    fixture.seed_tickets();
    let is_null = json!({ "where": [{ "field": "status", "operator": "isNull" }] });
    assert_eq!(fixture.titles(&is_null, first_page()), ["foxtrot", "golf"]);
    let eq_null = json!({ "where": [{ "field": "status", "operator": "eq", "value": null }] });
    assert_eq!(fixture.titles(&eq_null, first_page()), ["foxtrot", "golf"]);
    let not_null = json!({ "where": [{ "field": "status", "operator": "isNotNull" }] });
    assert_eq!(fixture.count(&not_null), 6);
    let not_open = json!({ "where": [{ "field": "status", "operator": "ne", "value": "open" }] });
    assert_eq!(fixture.titles(&not_open, first_page()), ["charlie", "echo"]);
}

#[test]
fn ordered_operators_compare_only_within_one_type() {
    let fixture = Fixture::new(CollectionLayout::Segments);
    fixture.seed_tickets();
    let at_least_three =
        json!({ "where": [{ "field": "priority", "operator": "gte", "value": 3 }] });
    assert_eq!(
        fixture.titles(&at_least_three, first_page()),
        ["alpha", "delta", "golf"]
    );
    let before_c = json!({ "where": [{ "field": "title", "operator": "lt", "value": "c" }] });
    assert_eq!(fixture.titles(&before_c, first_page()), ["alpha", "bravo"]);
}

fn check_in_and_and(layout: CollectionLayout) {
    let fixture = Fixture::new(layout);
    fixture.seed_tickets();
    let query = json!({ "where": [
        { "field": "priority", "operator": "in", "value": [1, 2, "3"] },
        { "field": "status", "operator": "ne", "value": "closed" }
    ] });
    assert_eq!(
        fixture.titles(&query, first_page()),
        ["bravo", "echo", "hotel"]
    );
}

#[test]
fn in_and_and_combine_predicates_in_files() {
    check_in_and_and(CollectionLayout::Files);
}

#[test]
fn in_and_and_combine_predicates_in_segments() {
    check_in_and_and(CollectionLayout::Segments);
}

#[test]
fn any_of_lists_are_joined_by_or() {
    let fixture = Fixture::new(CollectionLayout::Files);
    fixture.seed_tickets();
    let query = json!({
        "where": [{ "field": "title", "operator": "ne", "value": "alpha" }],
        "anyOf": [
            [{ "field": "priority", "operator": "eq", "value": 3 }],
            [{ "field": "status", "operator": "eq", "value": "closed" }]
        ]
    });
    assert_eq!(fixture.titles(&query, first_page()), ["charlie", "delta"]);
}

#[test]
fn sort_puts_null_first_then_booleans_numbers_and_strings() {
    let fixture = Fixture::new(CollectionLayout::Segments);
    fixture.seed_tickets();
    let ascending = json!({ "orderBy": [{ "field": "priority" }] });
    assert_eq!(
        fixture.titles(&ascending, first_page()),
        [
            "foxtrot", "bravo", "charlie", "hotel", "alpha", "delta", "golf", "echo"
        ]
    );
    let descending = json!({ "orderBy": [{ "field": "priority", "direction": "desc" }] });
    assert_eq!(
        fixture.titles(&descending, first_page()),
        [
            "echo", "golf", "alpha", "delta", "charlie", "hotel", "bravo", "foxtrot"
        ]
    );
}

#[test]
fn several_sort_fields_break_ties_in_order_then_by_id() {
    let fixture = Fixture::new(CollectionLayout::Files);
    fixture.seed_tickets();
    let query = json!({
        "where": [{ "field": "status", "operator": "eq", "value": "open" }],
        "orderBy": [{ "field": "priority", "direction": "desc" }, { "field": "title", "direction": "desc" }]
    });
    assert_eq!(
        fixture.titles(&query, first_page()),
        ["delta", "alpha", "hotel", "bravo"]
    );
}

#[test]
fn an_object_or_array_sort_key_is_refused() {
    let fixture = Fixture::new(CollectionLayout::Files);
    fixture.seed_tickets();
    let query = Fixture::query(&json!({ "orderBy": [{ "field": "tags" }] }));
    let error = fixture
        .reader()
        .store_find(Fixture::COLLECTION, &query, first_page(), &no_overlay())
        .unwrap_err();
    assert_eq!(error.query_code_str(), Some("EQUERY_INVALID"));
}

#[test]
fn offset_pages_follow_the_order() {
    let fixture = Fixture::new(CollectionLayout::Segments);
    fixture.seed_tickets();
    let query = json!({ "orderBy": [{ "field": "title", "direction": "desc" }] });
    assert_eq!(
        fixture.titles(&query, StorePage::new(3, 2)),
        ["foxtrot", "echo", "delta"]
    );
    assert_eq!(fixture.titles(&query, StorePage::new(3, 7)), ["alpha"]);
    assert!(fixture.titles(&query, StorePage::new(3, 50)).is_empty());
}

#[test]
fn the_id_field_can_be_queried_and_sorted() {
    let fixture = Fixture::new(CollectionLayout::Files);
    fixture.seed_tickets();
    let query = json!({
        "where": [{ "field": "id", "operator": "gte", "value": Fixture::id(6) }],
        "orderBy": [{ "field": "id", "direction": "desc" }]
    });
    assert_eq!(fixture.titles(&query, first_page()), ["hotel", "golf"]);
}

#[test]
fn pages_have_no_ten_thousand_document_bound() {
    let fixture = Fixture::new(CollectionLayout::Segments);
    let total = 10_050;
    let rows: Vec<Value> = (0..total)
        .map(|number| json!({ "number": number, "even": number % 2 == 0, "title": format!("n{number:05}") }))
        .collect();
    fixture.put_in_batches(&rows);
    assert_eq!(fixture.count(&json!({})), 10_050);
    assert_eq!(
        fixture.count(&json!({ "where": [{ "field": "even", "operator": "eq", "value": true }] })),
        5_025
    );
    let query = json!({ "orderBy": [{ "field": "number" }] });
    assert_eq!(
        fixture.titles(&query, StorePage::new(20, 10_040)),
        [
            "n10040", "n10041", "n10042", "n10043", "n10044", "n10045", "n10046", "n10047",
            "n10048", "n10049"
        ]
    );
    let late = json!({ "where": [{ "field": "number", "operator": "gte", "value": 10_048 }] });
    assert_eq!(fixture.titles(&late, first_page()), ["n10048", "n10049"]);
}

fn scroll_page(
    fixture: &Fixture,
    query: &StoreQuery,
    cursor: Option<String>,
) -> (Vec<String>, Option<String>) {
    let page = fixture
        .reader()
        .store_scroll(
            Fixture::COLLECTION,
            query,
            &StoreScrollPage::new(3, cursor),
            &no_overlay(),
        )
        .unwrap();
    let ids = page
        .content
        .iter()
        .map(|row| row.metadata.id.clone())
        .collect();
    (ids, page.next_cursor)
}

fn ids(numbers: &[usize]) -> Vec<String> {
    numbers.iter().copied().map(Fixture::id).collect()
}

fn check_scroll(layout: CollectionLayout) {
    let fixture = Fixture::new(layout);
    fixture.seed_tickets();
    let query =
        Fixture::query(&json!({ "orderBy": [{ "field": "priority", "direction": "desc" }] }));
    let (first, cursor) = scroll_page(&fixture, &query, None);
    assert_eq!(first, ids(&[4, 6, 0]));
    let (second, cursor) = scroll_page(&fixture, &query, cursor);
    assert_eq!(second, ids(&[3, 2, 7]));
    let (third, cursor) = scroll_page(&fixture, &query, cursor);
    assert_eq!(third, ids(&[1, 5]));
    assert_eq!(cursor, None);
}

#[test]
fn scroll_visits_every_match_once_in_order_in_files() {
    check_scroll(CollectionLayout::Files);
}

#[test]
fn scroll_visits_every_match_once_in_order_in_segments() {
    check_scroll(CollectionLayout::Segments);
}

#[test]
fn a_cursor_of_another_query_or_collection_is_refused() {
    let fixture = Fixture::new(CollectionLayout::Files);
    fixture.seed_tickets();
    let reader = fixture.reader();
    let by_title = Fixture::query(&json!({ "orderBy": [{ "field": "title" }] }));
    let first = reader
        .store_scroll(
            Fixture::COLLECTION,
            &by_title,
            &StoreScrollPage::new(2, None),
            &no_overlay(),
        )
        .unwrap();
    let cursor = first.next_cursor.unwrap();
    let by_priority = Fixture::query(&json!({ "orderBy": [{ "field": "priority" }] }));
    let error = reader
        .store_scroll(
            Fixture::COLLECTION,
            &by_priority,
            &StoreScrollPage::new(2, Some(cursor.clone())),
            &no_overlay(),
        )
        .unwrap_err();
    assert_eq!(error.query_code_str(), Some("ECURSOR"));
    let error = reader
        .store_scroll(
            Fixture::COLLECTION,
            &by_title,
            &StoreScrollPage::new(2, Some("fyc1.x.y".into())),
            &no_overlay(),
        )
        .unwrap_err();
    assert_eq!(error.query_code_str(), Some("ECURSOR"));
}

#[test]
fn exists_stops_early_and_answers_false_for_a_missing_collection() {
    let fixture = Fixture::new(CollectionLayout::Files);
    fixture.seed_tickets();
    let reader = fixture.reader();
    let closed = Fixture::query(
        &json!({ "where": [{ "field": "status", "operator": "eq", "value": "closed" }] }),
    );
    assert!(
        reader
            .store_exists(Fixture::COLLECTION, &closed, &no_overlay())
            .unwrap()
    );
    let pending = Fixture::query(
        &json!({ "where": [{ "field": "status", "operator": "eq", "value": "pending" }] }),
    );
    assert!(
        !reader
            .store_exists(Fixture::COLLECTION, &pending, &no_overlay())
            .unwrap()
    );
    assert!(
        !reader
            .store_exists("absent", &closed, &no_overlay())
            .unwrap()
    );
    let error = reader
        .store_count("absent", &closed, &no_overlay())
        .unwrap_err();
    assert_eq!(error.storage_code(), Some(NativeStorageErrorCode::NotFound));
}

fn check_delete(layout: CollectionLayout) {
    let fixture = Fixture::new(layout);
    fixture.seed_tickets();
    let open = Fixture::query(
        &json!({ "where": [{ "field": "status", "operator": "eq", "value": "open" }] }),
    );
    let matching = fixture
        .reader()
        .store_matching_ids(Fixture::COLLECTION, &open, &no_overlay())
        .unwrap();
    assert_eq!(matching, ids(&[0, 1, 3, 7]));
    let deleted = fixture
        .writer()
        .store_delete_documents(Fixture::COLLECTION, &matching)
        .unwrap();
    assert_eq!(deleted, 4);
    assert_eq!(fixture.count(&json!({})), 4);
    assert!(
        !fixture
            .reader()
            .store_exists(Fixture::COLLECTION, &open, &no_overlay())
            .unwrap()
    );
}

#[test]
fn delete_removes_exactly_the_matches_in_one_batch_in_files() {
    check_delete(CollectionLayout::Files);
}

#[test]
fn delete_removes_exactly_the_matches_in_one_batch_in_segments() {
    check_delete(CollectionLayout::Segments);
}

#[test]
fn index_narrowed_and_full_scans_answer_the_same() {
    let fixture = Fixture::new(CollectionLayout::Segments);
    let rows: Vec<Value> = (0..200)
        .map(|number| json!({ "group": number % 7, "label": format!("g{}", number % 3), "flag": number % 2 == 0 }))
        .collect();
    fixture.put_many(0, &rows);
    let narrowed = json!({ "where": [
        { "field": "group", "operator": "eq", "value": 3 },
        { "field": "label", "operator": "eq", "value": "g1" }
    ] });
    let full_scan = json!({ "where": [
        { "field": "group", "operator": "in", "value": [3, true] },
        { "field": "label", "operator": "like", "value": "g1" }
    ] });
    let expected = u64::try_from(
        (0..200)
            .filter(|number| number % 7 == 3 && number % 3 == 1)
            .count(),
    )
    .unwrap();
    assert_eq!(fixture.count(&narrowed), expected);
    assert_eq!(fixture.count(&full_scan), expected);
    let range = json!({ "where": [{ "field": "group", "operator": "gt", "value": 5 }] });
    let expected_range = u64::try_from((0..200).filter(|number| number % 7 > 5).count()).unwrap();
    assert_eq!(fixture.count(&range), expected_range);
}

#[test]
fn documents_have_their_fields_and_id() {
    let fixture = Fixture::new(CollectionLayout::Files);
    fixture.seed_tickets();
    let query = Fixture::query(
        &json!({ "where": [{ "field": "title", "operator": "eq", "value": "golf" }] }),
    );
    let rows = fixture
        .reader()
        .store_find(Fixture::COLLECTION, &query, first_page(), &no_overlay())
        .unwrap();
    let expected: Map<String, Value> = json!({ "priority": 5, "title": "golf" })
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].metadata.id, Fixture::id(6));
    assert_eq!(rows[0].document.fields(), &expected);
}

fn fields(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn an_overlay_adds_replaces_and_hides_documents_for_every_read() {
    let fixture = Fixture::new(CollectionLayout::Files);
    fixture.seed_tickets();
    let mut overlay = StoreOverlay::default();
    overlay.put(
        &Fixture::id(20),
        fields(&json!({ "status": "open", "priority": 9, "title": "india" })),
    );
    overlay.put(
        &Fixture::id(1),
        fields(&json!({ "status": "closed", "priority": 1, "title": "bravo" })),
    );
    overlay.delete(&Fixture::id(0));
    let reader = fixture.reader();
    let open = Fixture::query(&json!({
        "where": [{ "field": "status", "operator": "eq", "value": "open" }],
        "orderBy": [{ "field": "priority", "direction": "desc" }]
    }));
    let found: Vec<String> = reader
        .store_find(Fixture::COLLECTION, &open, first_page(), &overlay)
        .unwrap()
        .into_iter()
        .map(|row| row.document.fields()["title"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(found, ["india", "delta", "hotel"]);
    assert_eq!(
        reader
            .store_count(Fixture::COLLECTION, &open, &overlay)
            .unwrap(),
        3
    );
    let matching = reader
        .store_matching_ids(Fixture::COLLECTION, &open, &overlay)
        .unwrap();
    assert_eq!(matching, ids(&[3, 7, 20]));
    assert_eq!(fixture.count(&json!({})), 8);
}

#[test]
fn an_overlay_document_pages_and_scrolls_with_stored_ones() {
    let fixture = Fixture::new(CollectionLayout::Segments);
    fixture.seed_tickets();
    let mut overlay = StoreOverlay::default();
    overlay.put(&Fixture::id(20), fields(&json!({ "title": "aaa" })));
    let by_title = Fixture::query(&json!({ "orderBy": [{ "field": "title" }] }));
    let page = fixture
        .reader()
        .store_scroll(
            Fixture::COLLECTION,
            &by_title,
            &StoreScrollPage::new(2, None),
            &overlay,
        )
        .unwrap();
    let titles: Vec<&str> = page
        .content
        .iter()
        .map(|row| row.document.fields()["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["aaa", "alpha"]);
    assert!(page.next_cursor.is_some());
}
