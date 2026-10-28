//! Library-level validation tests.
//!
//! Ported from `tests/unit/validation.test.js` in the JavaScript build, case for
//! case, against the same schemas, kept in `tests/fixtures/`.

use fylo_chex::SchemaValidator;
use serde_json::{Map, Value, json};

fn example(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn valid_schema(name: &str) -> String {
    example(&format!("valid/{name}.schema.json"))
}

fn invalid_schema(name: &str) -> String {
    example(&format!("invalid/{name}.schema.json"))
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected a JSON object, got {other}"),
    }
}

/// Validate against an exact schema path, the way `Gen.validateData` did.
fn validate(schema_ref: &str, data: Value) -> fylo_chex::Result<()> {
    SchemaValidator::default().validate(schema_ref, &object(data))
}

/// Validate through name-based lookup in a schema directory.
fn validate_in_dir(dir: &str, name: &str, data: Value) -> fylo_chex::Result<()> {
    SchemaValidator::new(None, Some(dir.to_string())).validate(name, &object(data))
}

#[track_caller]
fn expect_error(result: fylo_chex::Result<()>, fragment: &str) {
    let error = result.expect_err("expected validation to fail");
    assert!(
        error.message.contains(fragment),
        "message {:?} does not contain {fragment:?}",
        error.message
    );
}

// ---------------------------------------------------------------------------
// validateData
// ---------------------------------------------------------------------------

fn valid_person() -> Value {
    json!({
        "name": "Jane Doe",
        "age": 30,
        "active": true,
        "nickname": null,
        "address": { "city": "Toronto", "country": "Canada" },
        "tags": ["typescript", "bun"],
        "scores": [95, 87],
        "meta": { "employer": "ACME", "dept": "engineering" },
    })
}

fn person_with(key: &str, value: Value) -> Value {
    let mut person = object(valid_person());
    person.insert(key.to_string(), value);
    Value::Object(person)
}

fn person_without(key: &str) -> Value {
    let mut person = object(valid_person());
    person.shift_remove(key);
    Value::Object(person)
}

#[test]
fn returns_the_validated_data_when_all_fields_are_valid() {
    assert!(validate(&valid_schema("person"), valid_person()).is_ok());
}

#[test]
fn skips_regex_validation_for_a_null_nullable_property() {
    assert!(
        validate(
            &valid_schema("person"),
            person_with("nickname", Value::Null)
        )
        .is_ok()
    );
}

#[test]
fn throws_for_a_property_not_defined_in_the_schema() {
    expect_error(
        validate(
            &valid_schema("person"),
            person_with("unknownField", json!("oops")),
        ),
        "Property 'unknownField' does not exist in schema",
    );
}

#[test]
fn throws_for_a_type_mismatch_on_an_array_property() {
    expect_error(
        validate(
            &valid_schema("person"),
            person_with("scores", json!("not-an-array")),
        ),
        "Type mismatch for 'scores' in schema",
    );
}

#[test]
fn throws_when_a_required_property_is_null() {
    expect_error(
        validate(&valid_schema("person"), person_with("age", Value::Null)),
        "Property 'age' cannot be null or undefined in schema",
    );
}

#[test]
fn throws_when_a_required_property_is_undefined() {
    expect_error(
        validate(&valid_schema("person"), person_without("age")),
        "Property 'age' cannot be null or undefined in schema",
    );
}

#[test]
fn throws_for_a_name_that_does_not_match_the_regex_pattern() {
    expect_error(
        validate(
            &valid_schema("person"),
            person_with("name", json!("madonna")),
        ),
        "RegEx pattern fails for property 'name' in schema",
    );
}

#[test]
fn accepts_a_name_that_matches_the_regex_pattern() {
    assert!(
        validate(
            &valid_schema("person"),
            person_with("name", json!("John Smith"))
        )
        .is_ok()
    );
}

#[test]
fn throws_for_an_array_element_that_does_not_match_the_regex_pattern() {
    expect_error(
        validate(
            &valid_schema("person"),
            person_with("tags", json!(["typescript", "BUN"])),
        ),
        "RegEx pattern fails for property 'tags' in schema",
    );
}

#[test]
fn throws_for_a_nested_property_regex_mismatch() {
    expect_error(
        validate(
            &valid_schema("person"),
            person_with("address", json!({ "city": "ABC123", "country": "Canada" })),
        ),
        "RegEx pattern fails for property 'address.city' in schema",
    );
}

#[test]
fn throws_for_a_record_entry_with_a_non_matching_value() {
    expect_error(
        validate(
            &valid_schema("person"),
            person_with("meta", json!({ "employer": "" })),
        ),
        "RegEx pattern fails for property 'meta.employer' in schema",
    );
}

#[test]
fn throws_for_a_record_entry_with_an_invalid_key() {
    expect_error(
        validate(
            &valid_schema("person"),
            person_with("meta", json!({ "123": "value" })),
        ),
        "RegEx pattern fails for property 'meta.<key:123>' in schema",
    );
}

#[test]
fn throws_when_the_schema_file_does_not_exist() {
    expect_error(
        validate(&valid_schema("nonexistent"), json!({ "x": "1" })),
        "Failed to load schema from",
    );
}

#[test]
fn throws_for_an_invalid_schema_name_when_resolving_through_a_schema_directory() {
    expect_error(
        validate_in_dir(&example("valid"), "bad@name", json!({})),
        "Invalid schema name",
    );
}

#[test]
fn can_validate_against_an_exact_schema_file_path() {
    let path = valid_schema("person");
    let mut validator = SchemaValidator::new(Some(path.clone()), None);
    assert!(validator.validate(&path, &object(valid_person())).is_ok());
}

#[test]
fn throws_when_the_schema_file_is_not_parseable_json() {
    expect_error(
        validate(&example("invalid/non-json.txt"), json!({})),
        "Schema path must point to a .schema.json file",
    );
}

#[test]
fn throws_when_the_schema_path_does_not_end_with_schema_json() {
    expect_error(
        validate(
            &example("invalid/wrong-extension.json"),
            json!({ "name": "Jane" }),
        ),
        "Schema path must point to a .schema.json file",
    );
}

#[test]
fn throws_when_the_schema_file_uses_jsonl_content() {
    expect_error(
        validate(&invalid_schema("json-lines"), json!({ "name": "Jane" })),
        "Schema files must contain one JSON object, not JSONL",
    );
}

#[test]
fn throws_when_a_schema_regex_pattern_is_empty() {
    expect_error(
        validate(&invalid_schema("empty-pattern"), json!({ "name": "Jane" })),
        "Schema pattern for 'name' in schema",
    );
}

#[test]
fn throws_when_a_schema_leaf_is_not_a_regex_string() {
    expect_error(
        validate(
            &invalid_schema("non-string-leaf"),
            json!({ "name": "Jane" }),
        ),
        "Schema value for 'name' in schema",
    );
}

#[test]
fn throws_when_an_array_schema_does_not_contain_an_item_template() {
    expect_error(
        validate(
            &invalid_schema("empty-array-pattern"),
            json!({ "tags": ["bun"] }),
        ),
        "Array schema for 'tags' in schema",
    );
}

#[test]
fn throws_when_an_array_schema_contains_more_than_one_item_template() {
    expect_error(
        validate(
            &invalid_schema("multiple-array-templates"),
            json!({ "items": [] }),
        ),
        "Array schema for 'items' in schema",
    );
}

#[test]
fn throws_when_an_array_object_schema_is_empty() {
    expect_error(
        validate(
            &invalid_schema("array-object-empty"),
            json!({ "items": [] }),
        ),
        "at 'items[]' must define at least one property",
    );
}

#[test]
fn throws_when_a_schema_regex_pattern_is_invalid() {
    expect_error(
        validate(
            &invalid_schema("invalid-regex-pattern"),
            json!({ "name": "Jane" }),
        ),
        "Invalid RegEx pattern for 'name' in schema",
    );
}

#[test]
fn throws_when_the_schema_object_is_empty() {
    expect_error(
        validate(&invalid_schema("empty-object"), json!({})),
        "must define at least one property",
    );
}

// ---------------------------------------------------------------------------
// validateData — arrays of objects
// ---------------------------------------------------------------------------

fn valid_order() -> Value {
    json!({
        "orderId": "ORD-1001",
        "items": [
            { "sku": "BOOK-1", "quantity": 2, "giftWrap": false },
            { "sku": "PEN-9", "quantity": 12, "giftWrap": true },
        ],
    })
}

fn order_with_items(items: Value) -> Value {
    let mut order = object(valid_order());
    order.insert("items".to_string(), items);
    Value::Object(order)
}

#[test]
fn passes_when_each_object_in_an_array_matches_the_object_template() {
    assert!(validate(&valid_schema("order"), valid_order()).is_ok());
}

#[test]
fn throws_when_an_item_in_an_object_array_is_not_an_object() {
    expect_error(
        validate(
            &valid_schema("order"),
            order_with_items(json!([{ "sku": "BOOK-1", "quantity": 2 }, "not-an-object"])),
        ),
        "Type mismatch for 'items[1]' in schema",
    );
}

#[test]
fn throws_when_an_object_array_item_has_a_property_that_fails_its_regex() {
    expect_error(
        validate(
            &valid_schema("order"),
            order_with_items(json!([{ "sku": "book-1", "quantity": 2 }])),
        ),
        "RegEx pattern fails for property 'items[0].sku' in schema",
    );
}

#[test]
fn throws_when_an_object_array_item_has_an_unknown_property() {
    expect_error(
        validate(
            &valid_schema("order"),
            order_with_items(json!([{ "sku": "BOOK-1", "quantity": 2, "color": "blue" }])),
        ),
        "Property 'color' does not exist in schema",
    );
}

// ---------------------------------------------------------------------------
// validateData — regex patterns
// ---------------------------------------------------------------------------

fn valid_status() -> Value {
    json!({ "direction": "north", "priority": 2, "label": "active", "tag": null })
}

fn status_with(key: &str, value: Value) -> Value {
    let mut status = object(valid_status());
    status.insert(key.to_string(), value);
    Value::Object(status)
}

#[test]
fn passes_when_all_regex_patterns_match() {
    assert!(validate(&valid_schema("status"), valid_status()).is_ok());
}

#[test]
fn throws_when_a_string_value_does_not_match_the_regex_pattern() {
    expect_error(
        validate(
            &valid_schema("status"),
            status_with("direction", json!("northwest")),
        ),
        "RegEx pattern fails for property 'direction' in schema",
    );
}

#[test]
fn skips_regex_check_for_a_null_nullable_field() {
    assert!(validate(&valid_schema("status"), status_with("tag", Value::Null)).is_ok());
}

#[test]
fn validates_a_non_null_value_against_a_nullable_regex_field() {
    assert!(validate(&valid_schema("status"), status_with("tag", json!("a"))).is_ok());
}

#[test]
fn throws_when_a_non_null_nullable_regex_field_has_an_invalid_value() {
    expect_error(
        validate(&valid_schema("status"), status_with("tag", json!("z"))),
        "RegEx pattern fails for property 'tag' in schema",
    );
}

// ---------------------------------------------------------------------------
// validateData — numeric and string constraints expressed as regexes
// ---------------------------------------------------------------------------

fn valid_measure() -> Value {
    json!({
        "score": 50,
        "temperature": -273.15,
        "quantity": 10,
        "username": "alice",
        "code": "AB12",
    })
}

fn measure_with(key: &str, value: Value) -> Value {
    let mut measure = object(valid_measure());
    measure.insert(key.to_string(), value);
    Value::Object(measure)
}

#[test]
fn passes_when_all_constrained_values_match_their_regex_patterns() {
    assert!(validate(&valid_schema("measure"), valid_measure()).is_ok());
}

// score: ^(100|[1-9]?[0-9])$ -> 0-100
#[test]
fn passes_at_the_minimum_boundary() {
    assert!(validate(&valid_schema("measure"), measure_with("score", json!(0))).is_ok());
}

#[test]
fn passes_at_the_maximum_boundary() {
    assert!(validate(&valid_schema("measure"), measure_with("score", json!(100))).is_ok());
}

#[test]
fn throws_when_score_is_above_maximum() {
    expect_error(
        validate(&valid_schema("measure"), measure_with("score", json!(101))),
        "RegEx pattern fails for property 'score' in schema",
    );
}

#[test]
fn throws_when_score_is_negative() {
    expect_error(
        validate(&valid_schema("measure"), measure_with("score", json!(-5))),
        "RegEx pattern fails for property 'score' in schema",
    );
}

// quantity: ^[0-9]*[05]$ -> multiples of 5
#[test]
fn passes_when_value_is_a_multiple_of_five() {
    assert!(
        validate(
            &valid_schema("measure"),
            measure_with("quantity", json!(25))
        )
        .is_ok()
    );
}

#[test]
fn throws_when_value_is_not_a_multiple_of_five() {
    expect_error(
        validate(&valid_schema("measure"), measure_with("quantity", json!(7))),
        "RegEx pattern fails for property 'quantity' in schema",
    );
}

// username: ^.{3,20}$ -> length 3-20
#[test]
fn passes_at_the_min_length_boundary() {
    assert!(
        validate(
            &valid_schema("measure"),
            measure_with("username", json!("abc"))
        )
        .is_ok()
    );
}

#[test]
fn throws_when_string_is_shorter_than_min_length() {
    expect_error(
        validate(
            &valid_schema("measure"),
            measure_with("username", json!("ab")),
        ),
        "RegEx pattern fails for property 'username' in schema",
    );
}

#[test]
fn throws_when_string_is_longer_than_max_length() {
    expect_error(
        validate(
            &valid_schema("measure"),
            measure_with("username", json!("a".repeat(21))),
        ),
        "RegEx pattern fails for property 'username' in schema",
    );
}

// code: ^.{4}$ -> exactly 4 characters
#[test]
fn passes_when_string_length_equals_exactly_four() {
    assert!(
        validate(
            &valid_schema("measure"),
            measure_with("code", json!("XY99"))
        )
        .is_ok()
    );
}

#[test]
fn throws_when_exact_length_field_is_too_short() {
    expect_error(
        validate(&valid_schema("measure"), measure_with("code", json!("AB1"))),
        "RegEx pattern fails for property 'code' in schema",
    );
}

#[test]
fn throws_when_exact_length_field_is_too_long() {
    expect_error(
        validate(
            &valid_schema("measure"),
            measure_with("code", json!("AB123")),
        ),
        "RegEx pattern fails for property 'code' in schema",
    );
}
