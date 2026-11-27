use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{MarkerFile, NamespaceName};
use crate::{NativeStorageError, NativeStorageErrorCode};

/// `<app>/<namespace>/.fylo/namespace.json`: marks a folder as a namespace
/// (ADR 0033 section 3, storage contract v2).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceMarker {
    format: String,
    name: NamespaceName,
}

impl NamespaceMarker {
    /// The format this marker names.
    pub const FORMAT: &'static str = "fylo.namespace.v1";

    /// A marker for namespace `name`.
    #[must_use]
    pub fn new(name: NamespaceName) -> Self {
        Self {
            format: Self::FORMAT.to_owned(),
            name,
        }
    }

    /// The marker at `path`, or `None` when the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_METADATA` when the file is not a valid namespace
    /// marker, and `ENATIVE_IO` when it cannot be read.
    pub fn read(path: &Path) -> Result<Option<Self>, NativeStorageError> {
        MarkerFile::read_if_present::<Self>(path)?
            .map(Self::validated)
            .transpose()
    }

    /// The namespace's name; it equals the folder name.
    #[must_use]
    pub fn name(&self) -> &NamespaceName {
        &self.name
    }

    /// Write the marker durably to `path`.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_IO` when the file cannot be written.
    pub fn write(&self, path: &Path) -> Result<(), NativeStorageError> {
        crate::write::write_json_durable(path, self)
    }

    fn validated(self) -> Result<Self, NativeStorageError> {
        if self.format != Self::FORMAT {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                format!(
                    "the namespace marker names the format `{}`, not `{}`",
                    self.format.escape_debug(),
                    Self::FORMAT
                ),
            ));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::NamespaceMarker;
    use crate::layout::NamespaceName;

    #[test]
    fn the_json_follows_the_storage_contract() {
        let marker = NamespaceMarker::new(NamespaceName::parse("slice-12").unwrap());
        assert_eq!(
            serde_json::to_string(&marker).unwrap(),
            r#"{"format":"fylo.namespace.v1","name":"slice-12"}"#
        );
        assert_eq!(marker.name().as_str(), "slice-12");
    }

    #[test]
    fn a_bad_name_in_the_file_is_refused() {
        let text = r#"{"format":"fylo.namespace.v1","name":"../up"}"#;
        assert!(serde_json::from_str::<NamespaceMarker>(text).is_err());
    }

    #[test]
    fn another_format_is_refused() {
        let marker: NamespaceMarker =
            serde_json::from_str(r#"{"format":"fylo.namespace.v2","name":"public"}"#).unwrap();
        assert_eq!(
            marker.validated().unwrap_err().code().as_str(),
            "ENATIVE_METADATA"
        );
    }
}
