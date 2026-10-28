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
    let upgrader = json!(fixture.path("people/upgraders/v1-to-v1.js"));
    assert_eq!(report["ok"], false);
    assert_eq!(report["warnings"], json!([]));
    assert_eq!(
        report["issues"],
        json!([
            format!("Missing schema version file: {path}"),
            format!("SHA-256 mismatch for v1: {path}"),
            format!("Missing upgrader v1->\"v1\": {upgrader}"),
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
fn materialize_runs_arrow_upgrader_and_removes_old_version() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    fixture.upgrader(
        b"export default doc => ({ ...doc, oldVersion: doc._v === undefined, name: 'Samuel' });",
    );
    let document = json!({"_v":"v1","name":"Sam"});
    assert_eq!(
        fixture
            .tools()
            .materialize("people", document.as_object().unwrap())
            .unwrap(),
        *json!({"_v":"v2","oldVersion":true,"name":"Samuel"})
            .as_object()
            .unwrap()
    );
}

#[test]
fn materialize_awaits_async_function_upgrader() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    fixture.upgrader(b"export default async function(doc) { return { ready: true }; }");
    assert_eq!(
        fixture
            .tools()
            .materialize("people", &Map::default())
            .unwrap(),
        *json!({"_v":"v2","ready":true}).as_object().unwrap()
    );
}

#[test]
fn materialize_rejects_a_non_callable_default_export() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    fixture.upgrader(b"export default 42;");
    assert_eq!(
        fixture
            .tools()
            .materialize("people", &Map::default())
            .unwrap_err(),
        format!(
            "Upgrader at {} must default-export an async (doc) => doc function",
            fixture.path("people/upgraders/v1-to-v2.js").display()
        )
    );
}

#[test]
fn materialize_rejects_a_non_object_return() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    fixture.upgrader(b"export default doc => [];");
    assert_eq!(
        fixture
            .tools()
            .materialize("people", &Map::default())
            .unwrap_err(),
        "Upgrader v1->v2 for 'people' must return an object"
    );
}

#[test]
fn materialize_reports_invalid_upgrader_utf8() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    fixture.upgrader(&[0xff]);
    assert_eq!(
        fixture
            .tools()
            .materialize("people", &Map::default())
            .unwrap_err(),
        format!(
            "upgrader {} is not valid UTF-8",
            fixture.path("people/upgraders/v1-to-v2.js").display()
        )
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
fn materialize_runs_each_version_in_manifest_order() {
    let fixture = SchemaFixture::new();
    fixture.manifest(&json!({"current":"v3","versions":[{"v":"v1"},{"v":"v2"},{"v":"v3"}]}));
    fixture.upgrader(b"export default doc => ({ order: 'first' });");
    fs::write(
        fixture.path("people/upgraders/v2-to-v3.js"),
        b"export default doc => ({ order: doc.order + '-second' });",
    )
    .unwrap();
    assert_eq!(
        fixture
            .tools()
            .materialize("people", &Map::default())
            .unwrap(),
        *json!({"order":"first-second","_v":"v3"})
            .as_object()
            .unwrap()
    );
}

#[test]
fn materialize_reports_rejected_promise() {
    let fixture = SchemaFixture::new();
    fixture.versions();
    fixture.upgrader(b"export default async function(doc) { throw new Error('rejected'); }");
    assert!(
        fixture
            .tools()
            .materialize("people", &Map::default())
            .unwrap_err()
            .starts_with("upgrader v1->v2 failed:")
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
