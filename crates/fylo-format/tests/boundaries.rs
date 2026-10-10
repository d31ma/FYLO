use fylo_format::{Document, DocumentLimits, FormatErrorCode, TtidTimestamps};
use serde_json::json;

struct FormatFixture;

impl FormatFixture {
    const INVALID_TIMESTAMP: &str = "identifier timestamp is outside the supported range";
    const ARRAY_DEPTH: usize = 2;
    const CODE_REPRESENTATIONS: usize = 2;
    const SMALL_NODE_LIMIT: usize = 2;
    const VALUE_COUNT: usize = 4;
    const INCOMPLETE_VALUE_COUNT: usize = 3;

    fn code_labels(code: FormatErrorCode) -> [String; FormatFixture::CODE_REPRESENTATIONS] {
        [
            code.as_str().to_owned(),
            serde_json::from_value(serde_json::to_value(code).unwrap()).unwrap(),
        ]
    }

    fn identifier_error(identifier: &str) -> (FormatErrorCode, String) {
        let error = TtidTimestamps::decode(identifier).unwrap_err();
        (error.code(), error.message().to_owned())
    }
}

#[test]
fn document_size_error_code_remains_stable() {
    assert_eq!(
        FormatFixture::code_labels(FormatErrorCode::DocumentTooLarge),
        ["EFORMAT_SIZE"; FormatFixture::CODE_REPRESENTATIONS]
    );
}

#[test]
fn json_error_code_remains_stable() {
    assert_eq!(
        FormatFixture::code_labels(FormatErrorCode::InvalidJson),
        ["EFORMAT_JSON"; FormatFixture::CODE_REPRESENTATIONS]
    );
}

#[test]
fn document_root_error_code_remains_stable() {
    assert_eq!(
        FormatFixture::code_labels(FormatErrorCode::DocumentRoot),
        ["EDOCUMENTROOT"; FormatFixture::CODE_REPRESENTATIONS]
    );
}

#[test]
fn array_of_objects_error_code_remains_stable() {
    assert_eq!(
        FormatFixture::code_labels(FormatErrorCode::ArrayOfObjects),
        ["EARRAYOFOBJECTS"; FormatFixture::CODE_REPRESENTATIONS]
    );
}

#[test]
fn document_depth_error_code_remains_stable() {
    assert_eq!(
        FormatFixture::code_labels(FormatErrorCode::DocumentTooDeep),
        ["EFORMAT_DEPTH"; FormatFixture::CODE_REPRESENTATIONS]
    );
}

#[test]
fn document_nodes_error_code_remains_stable() {
    assert_eq!(
        FormatFixture::code_labels(FormatErrorCode::TooManyNodes),
        ["EFORMAT_NODES"; FormatFixture::CODE_REPRESENTATIONS]
    );
}

#[test]
fn encoding_error_code_remains_stable() {
    assert_eq!(
        FormatFixture::code_labels(FormatErrorCode::Encode),
        ["EFORMAT_ENCODE"; FormatFixture::CODE_REPRESENTATIONS]
    );
}

#[test]
fn document_id_error_code_remains_stable() {
    assert_eq!(
        FormatFixture::code_labels(FormatErrorCode::InvalidDocumentId),
        ["EINVALIDDOCID"; FormatFixture::CODE_REPRESENTATIONS]
    );
}

#[test]
fn error_display_remains_stable() {
    let error = Document::parse(b"[]", DocumentLimits::default()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "EDOCUMENTROOT: FYLO document root must be a JSON object"
    );
}

#[test]
fn validation_preserves_limit_boundaries_and_error_precedence() {
    let source = br#"{"a":[1,null]}"#;
    let limits = DocumentLimits {
        max_bytes: source.len(),
        max_depth: FormatFixture::ARRAY_DEPTH,
        max_nodes: FormatFixture::VALUE_COUNT,
    };
    assert_eq!(
        Document::parse(source, limits).unwrap().encode().unwrap(),
        source
    );
    assert_eq!(
        Document::parse(
            source,
            DocumentLimits {
                max_bytes: source.len() - 1,
                ..limits
            }
        )
        .unwrap_err()
        .code(),
        FormatErrorCode::DocumentTooLarge
    );
    assert_eq!(
        Document::parse(
            source,
            DocumentLimits {
                max_depth: 1,
                ..limits
            }
        )
        .unwrap_err()
        .code(),
        FormatErrorCode::DocumentTooDeep
    );
    assert_eq!(
        Document::parse(
            source,
            DocumentLimits {
                max_nodes: FormatFixture::INCOMPLETE_VALUE_COUNT,
                ..limits
            }
        )
        .unwrap_err()
        .code(),
        FormatErrorCode::TooManyNodes
    );
    assert_eq!(
        Document::parse(
            b"{",
            DocumentLimits {
                max_bytes: 0,
                ..limits
            }
        )
        .unwrap_err()
        .code(),
        FormatErrorCode::DocumentTooLarge
    );
    assert_eq!(
        Document::try_from_value(
            json!({"a": [{}]}),
            DocumentLimits {
                max_depth: 1,
                ..limits
            }
        )
        .unwrap_err()
        .code(),
        FormatErrorCode::DocumentTooDeep
    );
    assert_eq!(
        Document::try_from_value(
            json!({"a": [{}]}),
            DocumentLimits {
                max_nodes: FormatFixture::SMALL_NODE_LIMIT,
                ..limits
            }
        )
        .unwrap_err()
        .code(),
        FormatErrorCode::TooManyNodes
    );
    assert!(
        Document::try_from_value(
            json!({}),
            DocumentLimits {
                max_nodes: 0,
                ..limits
            }
        )
        .is_ok()
    );
}

#[test]
fn ttid_validation_preserves_lifecycle_placeholders_and_diagnostics() {
    let creation = "4VRNF52JPCO";
    let timestamp = TtidTimestamps::decode(creation).unwrap().created_at;
    assert_eq!(
        TtidTimestamps::decode("4vrnf52jpco-x").unwrap().created_at,
        timestamp
    );
    assert_eq!(
        TtidTimestamps::decode("4VRNF52JPCO-X").unwrap().updated_at,
        None
    );
    let lifecycle = TtidTimestamps::decode("4VRNF52JPCO-4VRNF52JQHC-4VRNF52JQHC").unwrap();
    assert_eq!(lifecycle.updated_at, lifecycle.deleted_at);
}

#[test]
fn empty_identifier_preserves_diagnostic() {
    assert_eq!(
        FormatFixture::identifier_error(""),
        (
            FormatErrorCode::InvalidDocumentId,
            "identifier length is outside FYLO bounds".to_owned()
        )
    );
}

#[test]
fn extra_segment_identifier_preserves_diagnostic() {
    assert_eq!(
        FormatFixture::identifier_error("a-b-c-d"),
        (
            FormatErrorCode::InvalidDocumentId,
            "identifier has an invalid lifecycle shape".to_owned()
        )
    );
}

#[test]
fn empty_segment_identifier_preserves_diagnostic() {
    assert_eq!(
        FormatFixture::identifier_error("4VRNF52JPCO-"),
        (
            FormatErrorCode::InvalidDocumentId,
            "identifier contains an invalid segment".to_owned()
        )
    );
}

#[test]
fn creation_placeholder_identifier_preserves_diagnostic() {
    assert_eq!(
        FormatFixture::identifier_error("X"),
        (
            FormatErrorCode::InvalidDocumentId,
            "creation timestamp cannot be a placeholder".to_owned()
        )
    );
}

#[test]
fn deletion_placeholder_identifier_preserves_diagnostic() {
    assert_eq!(
        FormatFixture::identifier_error("4VRNF52JPCO-X-X"),
        (
            FormatErrorCode::InvalidDocumentId,
            "deletion timestamp cannot be a placeholder".to_owned()
        )
    );
}

#[test]
fn early_timestamp_identifier_preserves_diagnostic() {
    assert_eq!(
        FormatFixture::identifier_error("a"),
        (
            FormatErrorCode::InvalidDocumentId,
            FormatFixture::INVALID_TIMESTAMP.to_owned()
        )
    );
}

#[test]
fn late_timestamp_identifier_preserves_diagnostic() {
    assert_eq!(
        FormatFixture::identifier_error("zzzzzzzzzzz"),
        (
            FormatErrorCode::InvalidDocumentId,
            FormatFixture::INVALID_TIMESTAMP.to_owned()
        )
    );
}
