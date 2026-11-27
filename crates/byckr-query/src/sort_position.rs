use crate::index::encode_uri_component;

/// Where a sorted walk stands: the sortable value and document id of one
/// index key (ADR 0023 section 6).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SortPosition {
    value: String,
    id: String,
}

impl SortPosition {
    /// A position from a key's encoded value and document id.
    #[must_use]
    pub fn decoded(value: &str, encoded_id: &str) -> Option<Self> {
        Some(Self {
            value: value.to_owned(),
            id: Self::decode_id(encoded_id)?,
        })
    }

    /// The 16 hexadecimal digits of the sortable value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The document id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The index key of this position under `prefix`.
    #[must_use]
    pub fn key(&self, prefix: &str) -> String {
        format!("{prefix}{}/{}", self.value, encode_uri_component(&self.id))
    }

    fn decode_id(encoded: &str) -> Option<String> {
        let mut bytes = Vec::with_capacity(encoded.len());
        let mut rest = encoded.as_bytes();
        while let Some((&byte, tail)) = rest.split_first() {
            let (decoded, tail) = Self::decode_byte(byte, tail)?;
            bytes.push(decoded);
            rest = tail;
        }
        String::from_utf8(bytes).ok().filter(|id| !id.is_empty())
    }

    fn decode_byte(byte: u8, tail: &[u8]) -> Option<(u8, &[u8])> {
        if byte != b'%' {
            return Some((byte, tail));
        }
        let digits = std::str::from_utf8(tail.get(..2)?).ok()?;
        Some((u8::from_str_radix(digits, 16).ok()?, &tail[2..]))
    }
}
