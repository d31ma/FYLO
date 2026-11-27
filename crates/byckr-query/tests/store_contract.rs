//! The portable half of the Store query plan (`api/store/v1`): parsing and
//! limits, the strict predicate rules, the cross-type sort order, pages,
//! cursors and the index narrowing.

use std::cmp::Ordering;

use byckr_query::{
    QueryError, StoreNarrowing, StorePage, StorePredicate, StoreQuery, StoreRow, StoreScrollPage,
    StoreSortValue,
};
use serde_json::{Map, Value, json};

fn query(value: &Value) -> StoreQuery {
    StoreQuery::from_value(Some(value), StoreQuery::DEFAULT_ID_FIELD).unwrap()
}

fn query_error(value: &Value) -> QueryError {
    StoreQuery::from_value(Some(value), StoreQuery::DEFAULT_ID_FIELD).unwrap_err()
}

fn predicate(operator: &str, value: &Value) -> StorePredicate {
    StorePredicate::from_value(&json!({ "field": "f", "operator": operator, "value": value }))
        .unwrap()
}

fn document(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn predicate_list(count: usize) -> Value {
    Value::Array(vec![json!({ "field": "f", "operator": "isNull" }); count])
}

#[test]
fn an_empty_or_missing_query_matches_every_document() {
    let fields = document(&json!({ "a": 1 }));
    let row = StoreRow::new("4VRNF52J000", &fields);
    let missing = StoreQuery::from_value(None, "id").unwrap();
    assert!(missing.matches(&row));
    assert!(query(&json!({})).matches(&row));
}

#[test]
fn equality_is_numeric_for_numbers_and_never_across_types() {
    let equal = predicate("eq", &json!(1));
    assert!(equal.matches(Some(&json!(1.0))));
    assert!(!equal.matches(Some(&json!("1"))));
    assert!(!equal.matches(Some(&json!(true))));
    assert!(!equal.matches(Some(&json!([1]))));
}

#[test]
fn strings_compare_by_exact_code_points() {
    let equal = predicate("eq", &json!("Straße"));
    assert!(equal.matches(Some(&json!("Straße"))));
    assert!(!equal.matches(Some(&json!("STRASSE"))));
    assert!(!equal.matches(Some(&json!("straße"))));
    assert!(predicate("lt", &json!("b")).matches(Some(&json!("B"))));
}

#[test]
fn eq_null_is_is_null_and_ne_never_matches_null_or_missing() {
    assert!(predicate("eq", &Value::Null).matches(None));
    assert!(predicate("eq", &Value::Null).matches(Some(&Value::Null)));
    assert!(!predicate("ne", &Value::Null).matches(None));
    assert!(predicate("ne", &Value::Null).matches(Some(&json!(0))));
    assert!(!predicate("ne", &json!("x")).matches(None));
    assert!(!predicate("ne", &json!("x")).matches(Some(&Value::Null)));
    assert!(predicate("ne", &json!("x")).matches(Some(&json!("y"))));
    assert!(predicate("isNull", &Value::Null).matches(None));
    assert!(!predicate("isNotNull", &Value::Null).matches(Some(&Value::Null)));
}

#[test]
fn ordered_operators_never_match_another_type() {
    assert!(predicate("gt", &json!(2)).matches(Some(&json!(2.5))));
    assert!(!predicate("gt", &json!(2)).matches(Some(&json!("3"))));
    assert!(!predicate("lte", &json!(2)).matches(None));
    assert!(predicate("gte", &json!("m")).matches(Some(&json!("m"))));
}

#[test]
fn in_matches_any_listed_scalar_and_like_and_match_test_text() {
    let listed = predicate("in", &json!(["a", 2, null]));
    assert!(listed.matches(Some(&json!(2.0))));
    assert!(listed.matches(None));
    assert!(!listed.matches(Some(&json!("2"))));
    assert!(predicate("like", &json!("ab%")).matches(Some(&json!("abc"))));
    assert!(!predicate("like", &json!("ab%")).matches(Some(&json!("Abc"))));
    assert!(predicate("match", &json!("quick fox")).matches(Some(&json!("The quick brown fox"))));
}

#[test]
fn a_value_of_the_wrong_type_is_refused() {
    let bad = [
        json!({ "field": "f", "operator": "gt", "value": true }),
        json!({ "field": "f", "operator": "eq", "value": [1] }),
        json!({ "field": "f", "operator": "in", "value": [] }),
        json!({ "field": "f", "operator": "in", "value": [[1]] }),
        json!({ "field": "f", "operator": "isNull", "value": 1 }),
        json!({ "field": "f", "operator": "like", "value": 1 }),
        json!({ "field": "f", "operator": "between", "value": 1 }),
        json!({ "field": "", "operator": "eq", "value": 1 }),
        json!({ "field": "f", "operator": "eq", "value": 1, "extra": 1 }),
    ];
    let codes: Vec<&str> = bad
        .iter()
        .map(|predicate| {
            StorePredicate::from_value(predicate)
                .unwrap_err()
                .code()
                .as_str()
        })
        .collect();
    assert_eq!(codes, vec!["EQUERY_INVALID"; bad.len()]);
}

#[test]
fn limits_answer_equerylimit() {
    let too_many = json!({ "where": predicate_list(9) });
    assert_eq!(query_error(&too_many).code().as_str(), "EQUERYLIMIT");
    let split =
        json!({ "where": predicate_list(5), "anyOf": [predicate_list(2), predicate_list(2)] });
    assert_eq!(query_error(&split).code().as_str(), "EQUERYLIMIT");
    let sorts = json!({ "orderBy": vec![json!({ "field": "f" }); 5] });
    assert_eq!(query_error(&sorts).code().as_str(), "EQUERYLIMIT");
    let values: Vec<usize> = (0..101).collect();
    let wide_in = json!({ "where": [{ "field": "f", "operator": "in", "value": values }] });
    assert_eq!(query_error(&wide_in).code().as_str(), "EQUERYLIMIT");
    let lists = json!({ "anyOf": vec![json!([]); 9] });
    assert_eq!(query_error(&lists).code().as_str(), "EQUERYLIMIT");
}

#[test]
fn eight_predicates_are_accepted() {
    let full =
        json!({ "where": predicate_list(4), "anyOf": [predicate_list(2), predicate_list(2)] });
    assert_eq!(query(&full).all_of().len(), 4);
}

#[test]
fn a_query_above_64_kib_answers_equery_input_size() {
    let long = "x".repeat(70 * 1024);
    let large = json!({ "where": [{ "field": "f", "operator": "eq", "value": long }] });
    assert_eq!(query_error(&large).code().as_str(), "EQUERY_INPUT_SIZE");
}

#[test]
fn unknown_query_fields_and_directions_are_refused() {
    assert_eq!(
        query_error(&json!({ "limit": 1 })).code().as_str(),
        "EQUERY_INVALID"
    );
    let direction = json!({ "orderBy": [{ "field": "f", "direction": "up" }] });
    assert_eq!(query_error(&direction).code().as_str(), "EQUERY_INVALID");
    assert_eq!(query_error(&json!([])).code().as_str(), "EQUERY_INVALID");
}

#[test]
fn sort_values_order_null_then_booleans_numbers_and_strings() {
    let ordered = [
        StoreSortValue::Null,
        StoreSortValue::Boolean(false),
        StoreSortValue::Boolean(true),
        StoreSortValue::Number(-1.5),
        StoreSortValue::Number(10.0),
        StoreSortValue::Text("A".into()),
        StoreSortValue::Text("a".into()),
    ];
    let pairs: Vec<Ordering> = ordered
        .windows(2)
        .map(|pair| pair[0].compare(&pair[1]))
        .collect();
    assert_eq!(pairs, vec![Ordering::Less; ordered.len() - 1]);
}

#[test]
fn an_object_cannot_be_a_sort_key() {
    let error = StoreSortValue::from_field("f", Some(&json!({ "a": 1 }))).unwrap_err();
    assert_eq!(error.code().as_str(), "EQUERY_INVALID");
}

#[test]
fn positions_break_ties_by_id_ascending_in_both_directions() {
    let descending = query(&json!({ "orderBy": [{ "field": "n", "direction": "desc" }] }));
    let low = document(&json!({ "n": 1 }));
    let high = document(&json!({ "n": 2 }));
    let first = descending
        .position(&StoreRow::new("4VRNF52J000", &low))
        .unwrap();
    let second = descending
        .position(&StoreRow::new("4VRNF52J001", &low))
        .unwrap();
    let top = descending
        .position(&StoreRow::new("4VRNF52J002", &high))
        .unwrap();
    assert!(top < first);
    assert!(first < second);
}

#[test]
fn a_cursor_resumes_at_its_position_and_only_for_its_query_and_scope() {
    let by_name = query(&json!({ "orderBy": [{ "field": "name" }] }));
    let fields = document(&json!({ "name": "Zoë" }));
    let position = by_name
        .position(&StoreRow::new("4VRNF52J007", &fields))
        .unwrap();
    let cursor = by_name.cursor(&position, "public/people");
    let resumed = by_name.cursor_position(&cursor, "public/people").unwrap();
    assert_eq!(resumed, position);
    let elsewhere = by_name.cursor_position(&cursor, "public/pets").unwrap_err();
    assert_eq!(elsewhere.code().as_str(), "ECURSOR");
    let by_age = query(&json!({ "orderBy": [{ "field": "age" }] }));
    let other_query = by_age
        .cursor_position(&cursor, "public/people")
        .unwrap_err();
    assert_eq!(other_query.code().as_str(), "ECURSOR");
    let garbage = by_name
        .cursor_position("fyc1.zz", "public/people")
        .unwrap_err();
    assert_eq!(garbage.code().as_str(), "ECURSOR");
}

#[test]
fn a_scroll_page_scopes_its_cursor_to_a_namespace_and_a_collection() {
    let page = StoreScrollPage::new(3, None);
    assert_eq!(page.scope("people"), "public/people");
    let within = page.within("slice-12");
    assert_eq!(within.scope("people"), "slice-12/people");
}

#[test]
fn pages_default_to_twenty_and_check_their_sizes() {
    let first = StorePage::from_value(None).unwrap();
    assert_eq!((first.size(), first.offset()), (20, 0));
    let deep = StorePage::from_value(Some(&json!({ "size": 5, "offset": 50_000 }))).unwrap();
    assert_eq!(deep.end(), 50_005);
    let raised = StorePage::from_value(Some(&json!({ "size": 500, "maxSize": 1000 }))).unwrap();
    assert_eq!(raised.size(), 500);
    let small_max = StorePage::from_value(Some(&json!({ "maxSize": 5 }))).unwrap();
    assert_eq!(small_max.size(), 5);
    let sizes = [
        json!({ "size": 101 }),
        json!({ "size": 0 }),
        json!({ "maxSize": 1001 }),
    ];
    let codes: Vec<&str> = sizes
        .iter()
        .map(|page| {
            StorePage::from_value(Some(page))
                .unwrap_err()
                .code()
                .as_str()
        })
        .collect();
    assert_eq!(codes, ["EPAGESIZE"; 3]);
    let offset = StorePage::from_value(Some(&json!({ "offset": -1 }))).unwrap_err();
    assert_eq!(offset.code().as_str(), "EQUERY_INVALID");
}

#[test]
fn scroll_pages_take_a_cursor_or_null() {
    let first = StoreScrollPage::from_value(Some(&json!({ "size": 3, "cursor": null }))).unwrap();
    assert_eq!((first.size(), first.cursor()), (3, None));
    let next = StoreScrollPage::from_value(Some(&json!({ "cursor": "fyc1.a.b" }))).unwrap();
    assert_eq!(next.cursor(), Some("fyc1.a.b"));
    let wrong = StoreScrollPage::from_value(Some(&json!({ "offset": 3 }))).unwrap_err();
    assert_eq!(wrong.code().as_str(), "EQUERY_INVALID");
}

#[test]
fn narrowing_uses_equality_and_numeric_ranges() {
    let store = query(&json!({ "where": [
        { "field": "status", "operator": "eq", "value": "open" },
        { "field": "size", "operator": "gte", "value": 3 },
        { "field": "title", "operator": "gte", "value": "m" }
    ] }));
    let narrowed = StoreNarrowing::structured(&store).unwrap();
    let expected = json!({ "status": { "$eq": "open" }, "size": { "$gte": 3 } });
    assert_eq!(narrowed.operations(), [document(&expected)]);
}

#[test]
fn narrowing_expands_an_in_into_alternatives() {
    let store =
        query(&json!({ "where": [{ "field": "kind", "operator": "in", "value": ["a", 2] }] }));
    let narrowed = StoreNarrowing::structured(&store).unwrap();
    let expected = [
        document(&json!({ "kind": { "$eq": "a" } })),
        document(&json!({ "kind": { "$eq": 2 } })),
    ];
    assert_eq!(narrowed.operations(), expected);
}

#[test]
fn narrowing_gives_up_when_a_branch_has_nothing_the_index_answers() {
    let unplanned = [
        json!({}),
        json!({ "where": [{ "field": "status", "operator": "ne", "value": "open" }] }),
        json!({ "where": [{ "field": "flag", "operator": "eq", "value": true }] }),
        json!({ "where": [{ "field": "id", "operator": "eq", "value": "4VRNF52J000" }] }),
        json!({ "where": [{ "field": "a.b", "operator": "eq", "value": 1 }] }),
        json!({ "anyOf": [
            [{ "field": "status", "operator": "eq", "value": "open" }],
            [{ "field": "status", "operator": "isNull" }]
        ] }),
    ];
    let planned: Vec<bool> = unplanned
        .iter()
        .map(|value| StoreNarrowing::structured(&query(value)).is_some())
        .collect();
    assert_eq!(planned, vec![false; unplanned.len()]);
}

#[test]
fn narrowing_joins_where_into_each_any_of_list() {
    let store = query(&json!({
        "where": [{ "field": "team", "operator": "eq", "value": "red" }],
        "anyOf": [
            [{ "field": "level", "operator": "gt", "value": 5 }],
            [{ "field": "level", "operator": "lt", "value": 1 }]
        ]
    }));
    let narrowed = StoreNarrowing::structured(&store).unwrap();
    let expected = [
        document(&json!({ "team": { "$eq": "red" }, "level": { "$gt": 5 } })),
        document(&json!({ "team": { "$eq": "red" }, "level": { "$lt": 1 } })),
    ];
    assert_eq!(narrowed.operations(), expected);
}

#[test]
fn the_id_field_is_read_from_the_row() {
    let store = StoreQuery::from_value(
        Some(&json!({ "where": [{ "field": "key", "operator": "eq", "value": "4VRNF52J003" }] })),
        "key",
    )
    .unwrap();
    let fields = document(&json!({ "key": "something else" }));
    assert!(store.matches(&StoreRow::new("4VRNF52J003", &fields)));
    assert!(!store.matches(&StoreRow::new("4VRNF52J004", &fields)));
}
