use serde::Serialize;
use std::path::Path;

/// One damaged or unreadable file that a root verification found.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootDamage {
    /// The collection the file belongs to.
    pub collection: String,
    /// The file, or the collection folder when the whole collection could
    /// not be opened.
    pub file: String,
    /// The stable error code reading the file gave.
    pub code: String,
    /// What was wrong.
    pub message: String,
}

impl RootDamage {
    pub(crate) fn new(collection: &str, file: &Path, code: &str, message: String) -> Self {
        Self {
            collection: collection.to_owned(),
            file: file.display().to_string(),
            code: code.to_owned(),
            message,
        }
    }

    pub(crate) fn from_error(collection: &str, file: &Path, error: &crate::NativeStorageError) -> Self {
        Self::new(collection, file, error.code().as_str(), error.to_string())
    }
}
