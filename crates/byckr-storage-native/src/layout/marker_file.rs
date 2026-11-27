use std::io::ErrorKind;
use std::path::Path;

use byckr_vfs as fs;
use serde::Deserialize;

use crate::NativeStorageError;

/// Reads the small JSON marker files of the layout: `format.json` and
/// `namespace.json`.
pub(crate) struct MarkerFile;

impl MarkerFile {
    /// Most bytes a marker file may have.
    pub(crate) const MAX_BYTES: u64 = 4096;

    /// The marker at `path`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_METADATA` when the file is a link, not a plain file,
    /// too big or not the expected JSON, and `ENATIVE_IO` when it cannot be
    /// read.
    pub(crate) fn read_if_present<T: for<'de> Deserialize<'de>>(
        path: &Path,
    ) -> Result<Option<T>, NativeStorageError> {
        Self::is_present(path)?
            .then(|| crate::write::read_bounded_json(path, Self::MAX_BYTES))
            .transpose()
    }

    fn is_present(path: &Path) -> Result<bool, NativeStorageError> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if Self::means_missing(error.kind()) => Ok(false),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn means_missing(kind: ErrorKind) -> bool {
        matches!(kind, ErrorKind::NotFound | ErrorKind::NotADirectory)
    }
}
