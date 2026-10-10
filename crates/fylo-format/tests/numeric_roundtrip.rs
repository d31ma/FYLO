use fylo_format::{Document, DocumentLimits};
use serde_json::json;

struct NumberFixture;

impl NumberFixture {
    const LONG_INTEGER_DIGITS: usize = 83;
    const ENCODING_CYCLES: usize = 10;
    const GENERATED_VALUES: usize = 4096;
    const INITIAL_BITS: u64 = 0x2545_f491_4f6c_dd1d;
    const FIRST_SHIFT: u32 = 13;
    const SECOND_SHIFT: u32 = 7;
    const THIRD_SHIFT: u32 = 17;

    fn repeated_documents() -> (Vec<Document>, Vec<Document>) {
        let input = format!("{{\"\":4{}}}\n", "6".repeat(Self::LONG_INTEGER_DIGITS));
        let original = Document::parse(input.as_bytes(), DocumentLimits::default()).unwrap();
        let mut document = original.clone();
        let mut actual = Vec::new();
        for _ in 0..Self::ENCODING_CYCLES {
            document =
                Document::parse(&document.encode().unwrap(), DocumentLimits::default()).unwrap();
            actual.push(document.clone());
        }
        (actual, vec![original; Self::ENCODING_CYCLES])
    }

    fn generated_documents() -> [Vec<(u64, Document)>; 2] {
        let mut bits = Self::INITIAL_BITS;
        let mut expected = Vec::new();
        let mut actual = Vec::new();
        for _ in 0..Self::GENERATED_VALUES {
            bits ^= bits << Self::FIRST_SHIFT;
            bits ^= bits >> Self::SECOND_SHIFT;
            bits ^= bits << Self::THIRD_SHIFT;
            Self::append_finite_document(bits, &mut expected, &mut actual);
        }
        [actual, expected]
    }

    fn append_finite_document(
        bits: u64,
        expected: &mut Vec<(u64, Document)>,
        actual: &mut Vec<(u64, Document)>,
    ) {
        let value = f64::from_bits(bits);
        if value.is_finite() {
            let original =
                Document::try_from_value(json!({"value": value}), DocumentLimits::default())
                    .unwrap();
            let encoded = original.encode().unwrap();
            let reparsed = Document::parse(&encoded, DocumentLimits::default()).unwrap();
            expected.push((bits, original));
            actual.push((bits, reparsed));
        }
    }
}

#[test]
fn long_finite_integer_from_fuzz_artifact_preserves_canonical_value() {
    let (actual, expected) = NumberFixture::repeated_documents();
    assert_eq!(actual, expected);
}

#[test]
fn finite_float_values_survive_document_encoding() {
    let [actual, expected] = NumberFixture::generated_documents();
    assert_eq!(actual, expected);
}
