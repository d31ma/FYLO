use serde::{Deserialize, Serialize};

use crate::{NativeStorageError, NativeStorageErrorCode};

/// How a document collection keeps its documents on disk (ADR 0015).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionLayout {
    /// One JSON file per document under `docs/` and `.deleted/`.
    #[default]
    Files,
    /// Append-only segment files under `segments/`.
    Segments,
}

impl CollectionLayout {
    /// The name used in descriptors, settings and the CLI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Segments => "segments",
        }
    }

    /// Parse a layout name.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for a name that is not a layout.
    pub fn parse(name: &str) -> Result<Self, NativeStorageError> {
        match name {
            "files" => Ok(Self::Files),
            "segments" => Ok(Self::Segments),
            _ => Err(NativeStorageError::new(
                NativeStorageErrorCode::Unsupported,
                format!("unknown collection layout \"{name}\"; use \"files\" or \"segments\""),
            )),
        }
    }
}
