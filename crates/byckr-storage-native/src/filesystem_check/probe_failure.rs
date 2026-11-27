use std::path::Path;

use crate::{NativeStorageError, NativeStorageErrorCode};

/// Builds the `EFSPROBE` errors of the file-system probe. Each message names
/// the folder and the step that failed.
pub(crate) struct ProbeFailure;

impl ProbeFailure {
    pub(crate) fn from_io(
        step: &str,
        directory: &Path,
        error: &std::io::Error,
    ) -> NativeStorageError {
        Self::with_detail(step, directory, &error.to_string())
    }

    pub(crate) fn with_detail(step: &str, directory: &Path, detail: &str) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::FilesystemProbe,
            format!(
                "file-system probe failed in {}: {step}: {detail}",
                directory.display()
            ),
        )
    }
}
