use serde::{Deserialize, Serialize};

/// Stable format error codes exposed by compatibility fixtures and bindings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FormatErrorCode {
    /// Encoded document exceeded the configured byte limit.
    #[serde(rename = "EFORMAT_SIZE")]
    DocumentTooLarge,
    /// Input was not valid JSON.
    #[serde(rename = "EFORMAT_JSON")]
    InvalidJson,
    /// Document root was not an object.
    #[serde(rename = "EDOCUMENTROOT")]
    DocumentRoot,
    /// Document contained an array of objects.
    #[serde(rename = "EARRAYOFOBJECTS")]
    ArrayOfObjects,
    /// Document exceeded the configured nesting depth.
    #[serde(rename = "EFORMAT_DEPTH")]
    DocumentTooDeep,
    /// Document exceeded the configured value-node limit.
    #[serde(rename = "EFORMAT_NODES")]
    TooManyNodes,
    /// A validated value could not be encoded.
    #[serde(rename = "EFORMAT_ENCODE")]
    Encode,
    /// A FYLO document identifier was invalid.
    #[serde(rename = "EINVALIDDOCID")]
    InvalidDocumentId,
}

impl FormatErrorCode {
    /// Return the stable string representation used outside Rust.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.label()
    }

    const fn label(self) -> &'static str {
        match self {
            Self::DocumentTooLarge => "EFORMAT_SIZE",
            Self::InvalidJson => "EFORMAT_JSON",
            Self::DocumentRoot => "EDOCUMENTROOT",
            Self::ArrayOfObjects => "EARRAYOFOBJECTS",
            Self::DocumentTooDeep => "EFORMAT_DEPTH",
            Self::TooManyNodes => "EFORMAT_NODES",
            Self::Encode => "EFORMAT_ENCODE",
            Self::InvalidDocumentId => "EINVALIDDOCID",
        }
    }
}
