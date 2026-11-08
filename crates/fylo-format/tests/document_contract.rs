use serde_json::json;

use fylo_format::*;

struct ContractFixture;

impl ContractFixture {
    const SMALL_LIMIT: usize = 2;
    const CREATED_AT: u64 = 10;
    const UPDATED_AT: u64 = 30;
    const DOCUMENT_ID: &str = "01document";
    const OWNER: &str = "owner";
    const EDITORS: &str = "editors";
    const ID_FIELD: &str = "id";
    const CREATED_FIELD: &str = "createdAt";
    const UPDATED_FIELD: &str = "updatedAt";
    const MODIFIED_FIELD: &str = "mtime";
}

#[test]
fn document_round_trips_without_reordering_fields() {
    let source = br#"{"z":1,"a":{"city":"Toronto"},"tags":["a",2,true,null]}"#;
    let document = Document::parse(source, DocumentLimits::default()).unwrap();
    assert_eq!(document.encode().unwrap(), source);
}

#[test]
fn rejects_non_object_roots() {
    let error =
        Document::parse(br#"["not","a","document"]"#, DocumentLimits::default()).unwrap_err();
    assert_eq!(error.code(), FormatErrorCode::DocumentRoot);
}

#[test]
fn rejects_arrays_of_objects_with_field_path() {
    let source = br#"{"profile":{"members":[{"name":"Ada"}]}}"#;
    let error = Document::parse(source, DocumentLimits::default()).unwrap_err();
    assert_eq!(error.code(), FormatErrorCode::ArrayOfObjects);
    assert!(error.message().contains("profile/members"));
}

#[test]
fn rejects_nested_arrays_like_the_javascript_engine() {
    let error =
        Document::parse(br#"{"matrix":[[1,2],[3,4]]}"#, DocumentLimits::default()).unwrap_err();
    assert_eq!(error.code(), FormatErrorCode::ArrayOfObjects);
}

#[test]
fn enforces_byte_depth_and_node_limits() {
    let oversized = Document::parse(
        br#"{"name":"Ada"}"#,
        DocumentLimits {
            max_bytes: 4,
            ..DocumentLimits::default()
        },
    )
    .unwrap_err();
    assert_eq!(oversized.code(), FormatErrorCode::DocumentTooLarge);

    let deep = Document::parse(
        br#"{"a":{"b":{"c":1}}}"#,
        DocumentLimits {
            max_depth: ContractFixture::SMALL_LIMIT,
            ..DocumentLimits::default()
        },
    )
    .unwrap_err();
    assert_eq!(deep.code(), FormatErrorCode::DocumentTooDeep);

    let many = Document::parse(
        br#"{"a":1,"b":2}"#,
        DocumentLimits {
            max_nodes: ContractFixture::SMALL_LIMIT,
            ..DocumentLimits::default()
        },
    )
    .unwrap_err();
    assert_eq!(many.code(), FormatErrorCode::TooManyNodes);
}

#[test]
fn canonical_metadata_overrides_spoofed_custom_values() {
    let custom = json!({
        (ContractFixture::OWNER): ContractFixture::EDITORS,
        (ContractFixture::ID_FIELD): "spoofed",
        (ContractFixture::CREATED_FIELD): 0,
        (ContractFixture::UPDATED_FIELD): 0,
        (ContractFixture::MODIFIED_FIELD): 0
    })
    .as_object()
    .unwrap()
    .clone();
    let canonical = CanonicalMetadata {
        id: ContractFixture::DOCUMENT_ID.into(),
        created_at: ContractFixture::CREATED_AT,
        updated_at: ContractFixture::UPDATED_AT,
        mtime: ContractFixture::UPDATED_AT,
    };

    assert_eq!(
        canonical.merge_with_custom(&custom),
        json!({
            (ContractFixture::OWNER): ContractFixture::EDITORS,
            (ContractFixture::ID_FIELD): ContractFixture::DOCUMENT_ID,
            (ContractFixture::CREATED_FIELD): ContractFixture::CREATED_AT,
            (ContractFixture::UPDATED_FIELD): ContractFixture::UPDATED_AT,
            (ContractFixture::MODIFIED_FIELD): ContractFixture::UPDATED_AT
        })
        .as_object()
        .unwrap()
        .clone()
    );
}

#[test]
fn decodes_ttid_lifecycle_timestamps() {
    let timestamps = TtidTimestamps::decode("4VRNF52JPCO").unwrap();
    assert_eq!(timestamps.created_at, 1_785_099_771_859);
    assert_eq!(timestamps.updated_at, None);
    assert_eq!(timestamps.deleted_at, None);

    let deleted = TtidTimestamps::decode("4VRNF52JPCO-X-4VRNF52JQHC").unwrap();
    assert_eq!(deleted.created_at, timestamps.created_at);
    assert_eq!(deleted.updated_at, None);
    assert!(deleted.deleted_at.is_some());
}

#[test]
fn rejects_empty_identifier() {
    assert_eq!(
        TtidTimestamps::decode("").unwrap_err().code(),
        FormatErrorCode::InvalidDocumentId
    );
}

#[test]
fn rejects_traversal_identifier() {
    assert_eq!(
        TtidTimestamps::decode("../escape").unwrap_err().code(),
        FormatErrorCode::InvalidDocumentId
    );
}

#[test]
fn rejects_short_identifier() {
    assert_eq!(
        TtidTimestamps::decode("ABC").unwrap_err().code(),
        FormatErrorCode::InvalidDocumentId
    );
}

#[test]
fn rejects_deleted_placeholder_identifier() {
    assert_eq!(
        TtidTimestamps::decode("4VRNF52JPCO-X-X")
            .unwrap_err()
            .code(),
        FormatErrorCode::InvalidDocumentId
    );
}

#[test]
fn rejects_empty_segment_identifier() {
    assert_eq!(
        TtidTimestamps::decode("4VRNF52JPCO-").unwrap_err().code(),
        FormatErrorCode::InvalidDocumentId
    );
}
