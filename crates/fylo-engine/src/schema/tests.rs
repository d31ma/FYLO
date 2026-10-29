use std::fs;

use serde_json::{Map, Value, json};

use super::SchemaTools;

#[path = "schema_fixture.rs"]
mod schema_fixture;
use schema_fixture::SchemaFixture;

#[test]
fn absent_manifest_has_explicit_nulls_and_no_versions() {
    let fixture = SchemaFixture::new();
    assert_eq!(
        fixture.tools().inspect("people").unwrap(),
        json!({
            "collection":"people", "schemaDir":fixture.0,
            "versioned":false,"current":null,"manifest":null,"versions":[],
            "manifestPath":fixture.path("people/manifest.json")
        })
    );
}

#[test]
fn absent_manifest_does_not_materialize_or_validate() {
    let fixture = SchemaFixture::new();
    let document = json!({"name":"Sam","_v":"unknown"});
    let body = document.as_object().unwrap();
    assert_eq!(fixture.tools().materialize("people", body).unwrap(), *body);
    assert_eq!(
        fixture
            .tools()
            .validate_against_head("people", body)
            .unwrap(),
        None
    );
    assert_eq!(fixture.tools().current_version("people").unwrap(), None);
}

#[test]
fn inspection_preserves_optional_field_omissions_and_hashes() {
    let fixture = SchemaFixture::new();
    fixture.manifest(&json!({"current":"v1","versions":[{
        "v":"v1","addedAt":null,"sha256":null,"ignored":true
    }]}));
    let path = fixture.path("people/history/v1.schema.json");
    fs::write(&path, b"{}").unwrap();
    let inspection = fixture.tools().inspect("people").unwrap();
    assert_eq!(
        inspection["manifest"],
        json!({"current":"v1","versions":[{"v":"v1"}]})
    );
    assert_eq!(
        inspection["versions"],
        json!([{
            "version":"v1","current":true,"path":path,"exists":true,
            "actualSha256":"44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        }])
    );
}

#[test]
fn doctor_reports_findings_in_original_order() {
    let fixture = SchemaFixture::new();
    fixture.manifest(&json!({"current":"missing","versions":[
        {"v":"v1","sha256":"wrong"},{"v":"v1"}
    ]}));
    let report = fixture.tools().doctor("people");
    let path = json!(fixture.path("people/history/v1.schema.json"));
    let migration = json!(fixture.path("people/migrations/v1-to-v1.json"));
    assert_eq!(report["ok"], false);
    assert_eq!(report["warnings"], json!([]));
    assert_eq!(
        report["issues"],
        json!([
            format!("Missing schema version file: {path}"),
            format!("SHA-256 mismatch for v1: {path}"),
            format!("Missing migration v1->\"v1\": {migration}"),
            "Duplicate version label: v1",
            format!("Missing schema version file: {path}"),
            "Current version is not declared in manifest.versions: missing"
        ])
    );
}

#[test]
fn corrupt_manifest_is_a_doctor_issue_without_inspection() {
    let fixture = SchemaFixture::new();
    fs::write(fixture.path("people/manifest.json"), b"{").unwrap();
    let report = fixture.tools().doctor("people");
    assert_eq!(report["ok"], false);
    assert_eq!(report["inspect"], Value::Null);
    assert!(
        report["issues"][0]
            .as_str()
            .unwrap()
            .starts_with("schema manifest is corrupt:")
    );
}

#[test]
fn materialize_requires_a_nonempty_version_list() {
    let fixture = SchemaFixture::new();
    fixture.manifest(&json!({"current":"v1"}));
    assert_eq!(
        fixture
            .tools()
            .materialize("people", &Map::default())
            .unwrap_err(),
        "Invalid manifest for 'people': 'versions' must be a non-empty array"
    );
}

#[test]
fn unknown_document_version_precedes_missing_current_version() {
    let fixture = SchemaFixture::new();
    fixture.manifest(&json!({"current":"missing","versions":[{"v":"v1"}]}));
    let document = json!({"_v":"unknown"});
    assert_eq!(
        fixture
            .tools()
            .materialize("people", document.as_object().unwrap())
            .unwrap_err(),
        "Doc in 'people' is at unknown version 'unknown' (not in manifest.versions)"
    );
}

#[test]
fn materialize_rejects_missing_current_version() {
    let fixture = SchemaFixture::new();
    fixture.manifest(&json!({"current":"missing","versions":[{"v":"v1"}]}));
    assert_eq!(
        fixture
            .tools()
            .materialize("people", &Map::default())
            .unwrap_err(),
        "Manifest for 'people': 'current' (missing) is not present in 'versions'"
    );
}

#[test]
fn materialize_rejects_a_document_ahead_of_current() {
    let fixture = SchemaFixture::new();
    fixture.manifest(&json!({"current":"v1","versions":[{"v":"v1"},{"v":"v2"}]}));
    let document = json!({"_v":"v2"});
    assert_eq!(
        fixture
            .tools()
            .materialize("people", document.as_object().unwrap())
            .unwrap_err(),
        "Doc in 'people' is at v2, ahead of target v1: schema rolled back?"
    );
}

#[test]
fn current_document_without_a_version_remains_unstamped() {
    let fixture = SchemaFixture::new();
    fixture.manifest(&json!({"current":"v1","versions":[{"v":"v1"}]}));
    let document = json!({"name":"Sam"});
    assert_eq!(
        fixture
            .tools()
            .materialize("people", document.as_object().unwrap())
            .unwrap(),
        *document.as_object().unwrap()
    );
}

#[test]
fn validation_reports_missing_head_before_validating() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    assert_eq!(
        fixture
            .tools()
            .validate_against_head("people", &Map::default())
            .unwrap_err(),
        format!(
            "head schema is missing: {}",
            fixture.path("people/history/v2.schema.json").display()
        )
    );
}

#[cfg(unix)]
#[test]
fn manifest_symlink_is_rejected() {
    let fixture = SchemaFixture::new();
    let target = fixture.0.join("target.json");
    fs::write(&target, b"{}").unwrap();
    std::os::unix::fs::symlink(target, fixture.path("people/manifest.json")).unwrap();
    assert_eq!(
        fixture.tools().inspect("people").unwrap_err(),
        "schema path is not a regular file"
    );
}

#[test]
fn validation_returns_the_document_and_stamps_head_version() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    fixture.head_schema(&json!({"name":"^[A-Za-z]+$"}));
    let document = json!({"name":"Samuel"});
    assert_eq!(
        fixture
            .tools()
            .validate_against_head("people", document.as_object().unwrap())
            .unwrap(),
        Some(
            json!({"name":"Samuel","_v":"v2"})
                .as_object()
                .unwrap()
                .clone()
        )
    );
}

#[test]
fn flat_validation_does_not_stamp_a_version() {
    let fixture = SchemaFixture::new();
    fixture.flat_schema(&json!({"name":"^[A-Za-z]+$"}));
    let document = json!({"name":"Samuel"});
    assert_eq!(
        fixture
            .tools()
            .validate_against_head("people", document.as_object().unwrap())
            .unwrap(),
        Some(document.as_object().unwrap().clone())
    );
}

#[test]
fn flat_validation_reports_the_chex_message_for_the_collection() {
    let fixture = SchemaFixture::new();
    fixture.flat_schema(&json!({"name":"^[A-Za-z]+$"}));
    assert_eq!(
        fixture
            .tools()
            .validate_against_head("people", &Map::default())
            .unwrap_err(),
        "Property 'name' cannot be null or undefined in schema 'people'"
    );
}

#[test]
fn head_validation_reports_the_chex_message_for_the_head_file() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    fixture.head_schema(&json!({"name":"^[A-Za-z]+$"}));
    let document = json!({"name":"Sam 2"});
    assert_eq!(
        fixture
            .tools()
            .validate_against_head("people", document.as_object().unwrap())
            .unwrap_err(),
        format!(
            "RegEx pattern fails for property 'name' in schema '{}'",
            fixture.path("people/history/v2.schema.json").display()
        )
    );
}

#[test]
fn validation_rejects_a_property_the_schema_does_not_declare() {
    let fixture = SchemaFixture::new();
    fixture.flat_schema(&json!({"name":"^[A-Za-z]+$"}));
    let document = json!({"name":"Samuel","age":30});
    assert_eq!(
        fixture
            .tools()
            .validate_against_head("people", document.as_object().unwrap())
            .unwrap_err(),
        "Property 'age' does not exist in schema 'people'"
    );
}

#[test]
fn validation_rejects_a_malformed_schema() {
    let fixture = SchemaFixture::new();
    fixture.flat_schema(&json!({"name":""}));
    assert_eq!(
        fixture
            .tools()
            .validate_against_head("people", &Map::default())
            .unwrap_err(),
        "Schema pattern for 'name' in schema 'people' cannot be empty"
    );
}

#[test]
fn validation_removes_the_version_field_before_checking() {
    let fixture = SchemaFixture::new();
    fixture.flat_schema(&json!({"name":"^[A-Za-z]+$"}));
    let document = json!({"_v":"old","name":"Sam"});
    assert_eq!(
        fixture
            .tools()
            .validate_against_head("people", document.as_object().unwrap())
            .unwrap(),
        Some(json!({"name":"Sam"}).as_object().unwrap().clone())
    );
}

#[test]
fn validation_reads_a_schema_edited_between_calls() {
    let fixture = SchemaFixture::new();
    let tools = fixture.tools();
    let document = json!({"name":"Sam","age":30});
    fixture.flat_schema(&json!({"name":"^[A-Za-z]+$"}));
    assert!(
        tools
            .validate_against_head("people", document.as_object().unwrap())
            .is_err()
    );
    fixture.flat_schema(&json!({"name":"^[A-Za-z]+$","age?":"^[0-9]+$"}));
    assert_eq!(
        tools
            .validate_against_head("people", document.as_object().unwrap())
            .unwrap(),
        Some(document.as_object().unwrap().clone())
    );
}

#[allow(clippy::needless_pass_by_value)] // lets tests pass `json!` literals
fn materialized(fixture: &SchemaFixture, document: Value) -> Result<Value, String> {
    fixture
        .tools()
        .materialize("people", document.as_object().unwrap())
        .map(Value::Object)
}

#[allow(clippy::needless_pass_by_value)]
fn migrated(steps: Value, document: Value) -> Result<Value, String> {
    let fixture = SchemaFixture::new();
    fixture.versions();
    fixture.steps(&steps);
    materialized(&fixture, document)
}

#[test]
fn materialize_runs_steps_on_the_unversioned_document_and_stamps_head() {
    assert_eq!(
        migrated(
            json!([{"set":"oldVersion","value":true},{"rename":"name","to":"fullName"}]),
            json!({"_v":"v1","name":"Sam"})
        )
        .unwrap(),
        json!({"_v":"v2","oldVersion":true,"fullName":"Sam"})
    );
}

#[test]
fn materialize_runs_each_version_in_manifest_order() {
    let fixture = SchemaFixture::new();
    fixture.manifest(&json!({"current":"v3","versions":[{"v":"v1"},{"v":"v2"},{"v":"v3"}]}));
    fixture.steps(&json!([{"set":"order","value":"first"}]));
    fs::write(
        fixture.path("people/migrations/v2-to-v3.json"),
        br#"{"steps":[{"template":"order","value":"{order}-second"}]}"#,
    )
    .unwrap();
    assert_eq!(
        materialized(&fixture, json!({})).unwrap(),
        json!({"order":"first-second","_v":"v3"})
    );
}

#[test]
fn materialize_reports_a_missing_migration() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    assert_eq!(
        materialized(&fixture, json!({})).unwrap_err(),
        format!(
            "Missing migration v1->v2 for collection 'people' at {}",
            fixture.path("people/migrations/v1-to-v2.json").display()
        )
    );
}

#[test]
fn materialize_refuses_a_javascript_upgrader() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    fixture.legacy_upgrader(b"export default doc => doc;");
    let error = materialized(&fixture, json!({})).unwrap_err();
    assert!(error.contains("JavaScript upgrader v1->v2"), "{error}");
    assert!(error.contains("ADR 0018"), "{error}");
}

#[test]
fn materialize_reports_an_invalid_migration_file() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    let path = fixture.path("people/migrations/v1-to-v2.json");
    for (source, expected) in [
        (&b"{"[..], "is not valid JSON"),
        (br"[]", "must be an object"),
        (br#"{"steps":[],"extra":1}"#, "does not take \"extra\""),
        (
            br#"{"steps":[{"rename":"a"}]}"#,
            "step 1: migration step field \"to\" must be a string",
        ),
        (
            br#"{"steps":[{"rename":"a","to":"b","value":1}]}"#,
            "does not take \"value\"",
        ),
        (
            br#"{"steps":[{"set":"a","remove":"a","value":1}]}"#,
            "use one operation per step",
        ),
        (br#"{"steps":[{"eval":"a"}]}"#, "names no known operation"),
        (br#"{"steps":[{"remove":"a//b"}]}"#, "empty segment"),
        (
            br#"{"steps":[{"convert":"a","to":"date"}]}"#,
            "cannot convert to \"date\"",
        ),
    ] {
        fixture.migration(source);
        let error = materialized(&fixture, json!({})).unwrap_err();
        assert!(
            error.starts_with(&format!("migration {}", path.display())) && error.contains(expected),
            "{error}"
        );
    }
}

#[test]
fn rename_copy_and_remove_reach_nested_fields() {
    assert_eq!(
        migrated(
            json!([
                {"rename":"profile/first","to":"name/given"},
                {"copy":"name/given","to":"display"},
                {"remove":"profile/legacy"},
                {"rename":"missing","to":"ignored"}
            ]),
            json!({"profile":{"first":"Sam","legacy":1}})
        )
        .unwrap(),
        json!({"profile":{},"name":{"given":"Sam"},"display":"Sam","_v":"v2"})
    );
}

#[test]
fn a_path_through_a_non_object_fails() {
    let error = migrated(
        json!([{"set":"name/given","value":"x"}]),
        json!({"name":"Sam"}),
    )
    .unwrap_err();
    assert!(error.contains("crosses a non-object"), "{error}");
}

#[test]
fn default_fills_only_missing_or_null_fields() {
    assert_eq!(
        migrated(
            json!([
                {"default":"a","value":1},
                {"default":"b","value":2},
                {"default":"c","value":3}
            ]),
            json!({"a":null,"b":false})
        )
        .unwrap(),
        json!({"a":1,"b":false,"c":3,"_v":"v2"})
    );
}

#[test]
fn convert_follows_javascript_string_and_strict_number_and_boolean_rules() {
    assert_eq!(
        migrated(
            json!([
                {"convert":"age","to":"number"},
                {"convert":"score","to":"string"},
                {"convert":"whole","to":"string"},
                {"convert":"active","to":"boolean"},
                {"convert":"flag","to":"number"},
                {"convert":"none","to":"number"}
            ]),
            json!({"age":" 42 ","score":1.5,"whole":2.0,"active":"true","flag":true,"none":null})
        )
        .unwrap(),
        json!({"age":42,"score":"1.5","whole":"2","active":true,"flag":1,"none":null,"_v":"v2"})
    );
    for (step, document) in [
        (
            json!({"convert":"age","to":"number"}),
            json!({"age":"forty"}),
        ),
        (
            json!({"convert":"active","to":"boolean"}),
            json!({"active":"yes"}),
        ),
        (
            json!({"convert":"tags","to":"string"}),
            json!({"tags":["a"]}),
        ),
    ] {
        let error = migrated(json!([step]), document).unwrap_err();
        assert!(error.contains("cannot convert"), "{error}");
    }
}

#[test]
fn map_replaces_listed_values_and_falls_back_to_else() {
    let steps = json!([{"map":"status","values":{"1":"active","0":"inactive"},"else":"unknown"}]);
    assert_eq!(
        migrated(steps.clone(), json!({"status":1})).unwrap()["status"],
        "active"
    );
    assert_eq!(
        migrated(steps.clone(), json!({"status":"7"})).unwrap()["status"],
        "unknown"
    );
    assert_eq!(migrated(steps, json!({})).unwrap(), json!({"_v":"v2"}));
    assert_eq!(
        migrated(
            json!([{"map":"status","values":{"1":"active"}}]),
            json!({"status":2})
        )
        .unwrap()["status"],
        2
    );
}

#[test]
fn wrap_puts_a_scalar_in_an_array_once() {
    assert_eq!(
        migrated(
            json!([{"wrap":"tag"},{"wrap":"tags"}]),
            json!({"tag":"a","tags":["b"]})
        )
        .unwrap(),
        json!({"tag":["a"],"tags":["b"],"_v":"v2"})
    );
}

#[test]
fn string_steps_edit_text_and_reject_other_types() {
    assert_eq!(
        migrated(
            json!([
                {"trim":"name"},
                {"lower":"email"},
                {"upper":"code"},
                {"replace":"phone","find":"-","with":""},
                {"lower":"missing"}
            ]),
            json!({"name":"  Sam  ","email":"Sam@Example.COM","code":"straße","phone":"555-01-23"})
        )
        .unwrap(),
        json!({"name":"Sam","email":"sam@example.com","code":"STRASSE","phone":"5550123","_v":"v2"})
    );
    let error = migrated(json!([{"lower":"age"}]), json!({"age":3})).unwrap_err();
    assert!(error.contains("needs a string at age"), "{error}");
    let error = migrated(json!([{"replace":"a","find":"","with":"x"}]), json!({})).unwrap_err();
    assert!(error.contains("non-empty \"find\""), "{error}");
}

#[test]
fn template_fills_placeholders_and_keeps_doubled_braces() {
    assert_eq!(
        migrated(
            json!([{"template":"label","value":"{{{name/first}}} {name/last} #{n}{missing}"}]),
            json!({"name":{"first":"Sam","last":"Delma"},"n":7})
        )
        .unwrap()["label"],
        "{Sam} Delma #7"
    );
    for template in ["{open", "close}"] {
        let error =
            migrated(json!([{"template":"label","value":template}]), json!({})).unwrap_err();
        assert!(error.contains("migration template"), "{error}");
    }
}

#[test]
fn slugify_matches_the_retired_javascript_slug_upgrader() {
    for (title, slug) in [
        (json!("Hello, World!"), "hello-world"),
        (json!("  --Rust & Bun 2026-- "), "rust-bun-2026"),
        (json!("Crème brûlée"), "cr-me-br-l-e"),
        (json!("!!!"), "untitled"),
        (json!(null), "untitled"),
        (json!(42), "42"),
    ] {
        assert_eq!(
            migrated(
                json!([{"slugify":"slug","from":"title"}]),
                json!({"title":title})
            )
            .unwrap()["slug"],
            slug
        );
    }
    assert_eq!(
        migrated(json!([{"slugify":"slug","from":"title"}]), json!({})).unwrap()["slug"],
        "untitled"
    );
}
