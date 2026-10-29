use serde::{Deserialize, Serialize};

/// What a collection's writes do about its schema (ADR 0017 section 2).
///
/// The mode is recorded in the collection's catalog descriptor as
/// `"validation"`. A descriptor that records none runs under the host's
/// `strict_schema` setting, as every collection did before modes existed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationMode {
    /// Nothing is checked.
    Off,
    /// Every write is checked; a document the schema rejects is still
    /// written, and the violation is counted and logged.
    Warn,
    /// Every write is checked; a document the schema rejects fails the write
    /// with `ESCHEMA` and nothing is written.
    Strict,
}

impl ValidationMode {
    /// Parse a mode as the CLI and the machine protocol spell it.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "warn" => Some(Self::Warn),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }

    /// The spelling `parse` accepts and the descriptor records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Warn => "warn",
            Self::Strict => "strict",
        }
    }

    /// The mode a collection runs under: the one its descriptor records, or
    /// for a descriptor that records none, `strict` when the host opened the
    /// root with `strict_schema` and `off` otherwise.
    #[must_use]
    pub const fn resolve(recorded: Option<Self>, strict_schema: bool) -> Self {
        match recorded {
            Some(mode) => mode,
            None if strict_schema => Self::Strict,
            None => Self::Off,
        }
    }
}
