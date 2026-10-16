use fylo_format::{CanonicalMetadata, Document, DocumentLimits, FormatError, FormatErrorCode};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    schema_version: u32,
    producer: Producer,
    documents: Vec<DocumentCase>,
    encoded_documents: Vec<EncodedDocumentCase>,
    metadata: Vec<MetadataCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Producer {
    document_format: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentCase {
    name: String,
    input: Value,
    valid: bool,
    encoded: Option<String>,
    error_code: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EncodedDocumentCase {
    name: String,
    input: String,
    valid: bool,
    encoded: Option<String>,
    error_code: Option<String>,
    #[serde(default)]
    limits: DocumentLimitOverrides,
}

#[derive(Default, Deserialize)]
struct DocumentLimitOverrides {
    #[serde(rename = "maxBytes")]
    bytes: Option<usize>,
    #[serde(rename = "maxDepth")]
    depth: Option<usize>,
    #[serde(rename = "maxNodes")]
    nodes: Option<usize>,
}

impl DocumentLimitOverrides {
    fn apply(&self) -> DocumentLimits {
        let defaults = DocumentLimits::default();
        DocumentLimits {
            max_bytes: self.bytes.unwrap_or(defaults.max_bytes),
            max_depth: self.depth.unwrap_or(defaults.max_depth),
            max_nodes: self.nodes.unwrap_or(defaults.max_nodes),
        }
    }
}

#[derive(Deserialize)]
struct MetadataCase {
    name: String,
    custom: Map<String, Value>,
    canonical: CanonicalMetadata,
    expected: Map<String, Value>,
}

impl Fixture {
    const SCHEMA_VERSION: u32 = 1;

    fn has_error_prefix(codes: &[FormatErrorCode]) -> bool {
        codes.iter().all(|code| code.as_str().starts_with('E'))
    }

    fn verify_result(
        name: &str,
        result: Result<Document, FormatError>,
        expected: (bool, Option<String>, Option<String>),
    ) {
        let (valid, encoded, error_code) = expected;
        if valid {
            let document =
                result.unwrap_or_else(|error| panic!("{name} unexpectedly failed: {error}"));
            assert_eq!(
                String::from_utf8(document.encode().unwrap()).unwrap(),
                encoded.unwrap(),
                "{name}"
            );
            return;
        }
        let error = result.expect_err(name);
        assert_eq!(error.code().as_str(), error_code.unwrap(), "{name}");
    }

    fn verify() -> (usize, usize) {
        let fixture: Fixture =
            serde_json::from_str(include_str!("../../../tests/fixtures/rust-format-v1.json"))
                .unwrap();
        assert_eq!(fixture.schema_version, Self::SCHEMA_VERSION);
        assert_eq!(fixture.producer.document_format, Document::FORMAT_V1);
        let expected =
            fixture.documents.len() + fixture.encoded_documents.len() + fixture.metadata.len();
        let mut checked = 0;

        for test_case in fixture.documents {
            checked += 1;
            let result = Document::try_from_value(test_case.input, DocumentLimits::default());
            Self::verify_result(
                &test_case.name,
                result,
                (test_case.valid, test_case.encoded, test_case.error_code),
            );
        }

        for test_case in fixture.encoded_documents {
            checked += 1;
            let result = Document::parse(test_case.input.as_bytes(), test_case.limits.apply());
            Self::verify_result(
                &test_case.name,
                result,
                (test_case.valid, test_case.encoded, test_case.error_code),
            );
        }

        for test_case in fixture.metadata {
            checked += 1;
            assert_eq!(
                test_case.canonical.merge_with_custom(&test_case.custom),
                test_case.expected,
                "{}",
                test_case.name
            );
        }
        (checked, expected)
    }
}

#[test]
fn javascript_v1_fixture_matches_rust_format_contract() {
    let (checked, expected) = Fixture::verify();
    assert_eq!(checked, expected);
}

#[test]
fn stable_error_code_strings_are_complete() {
    let codes = [
        FormatErrorCode::DocumentTooLarge,
        FormatErrorCode::InvalidJson,
        FormatErrorCode::DocumentRoot,
        FormatErrorCode::ArrayOfObjects,
        FormatErrorCode::DocumentTooDeep,
        FormatErrorCode::TooManyNodes,
        FormatErrorCode::Encode,
        FormatErrorCode::InvalidDocumentId,
    ];
    assert!(Fixture::has_error_prefix(&codes));
}
