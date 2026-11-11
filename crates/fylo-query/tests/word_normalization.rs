//! The Rust word normalizer agrees with the shared fixture that the
//! JavaScript engine's tests also read (ADR 0023 section 2).

use fylo_query::WordText;
use serde_json::Value;

#[test]
fn words_match_the_shared_normalization_fixture() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/word-normalization-v1.json"
    ))
    .unwrap();
    assert_eq!(
        fixture["maxWordBytes"].as_u64(),
        Some(WordText::MAX_WORD_BYTES as u64)
    );
    for case in fixture["cases"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let expected: Vec<&str> = case["words"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| word.as_str().unwrap())
            .collect();
        assert_eq!(WordText::words(text), expected, "{text:?}");
    }
}
