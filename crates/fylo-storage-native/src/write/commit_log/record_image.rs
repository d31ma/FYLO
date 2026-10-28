use std::path::Path;

use fylo_vfs as fs;

use super::super::{CapturedAttribute, capture_attributes};
use crate::{NativeStorageError, path_exists_no_follow};

/// A record file as it was before a commit, kept in memory so a live writer
/// can roll back a failed apply without logging undo images.
#[derive(Clone, Debug)]
pub(crate) struct RecordImage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) attributes: Option<Vec<CapturedAttribute>>,
    pub(crate) mode: Option<u32>,
    pub(crate) modified_ms: u64,
}

impl RecordImage {
    /// Read the current image, or `None` when the file does not exist.
    pub(crate) fn read(path: &Path) -> Result<Option<Self>, NativeStorageError> {
        if !path_exists_no_follow(path)? {
            return Ok(None);
        }
        let metadata = fs::symlink_metadata(path).map_err(NativeStorageError::io)?;
        Ok(Some(Self {
            bytes: fs::read(path).map_err(NativeStorageError::io)?,
            attributes: Some(capture_attributes(path)?),
            mode: Self::mode_of(&metadata),
            modified_ms: Self::modified_ms_of(&metadata)?,
        }))
    }

    /// An image of bytes and modification time alone, for a commit that
    /// leaves attributes and mode untouched.
    pub(crate) fn contents(bytes: Vec<u8>, modified_ms: u64) -> Self {
        Self {
            bytes,
            attributes: None,
            mode: None,
            modified_ms,
        }
    }

    pub(crate) fn modified_ms_of(metadata: &fs::Metadata) -> Result<u64, NativeStorageError> {
        let modified = metadata.modified().map_err(NativeStorageError::io)?;
        let millis = modified
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| {
                NativeStorageError::new(
                    crate::NativeStorageErrorCode::Io,
                    format!("file time is before the Unix epoch: {error}"),
                )
            })?
            .as_millis();
        u64::try_from(millis).map_err(|_| {
            NativeStorageError::new(
                crate::NativeStorageErrorCode::Io,
                "file time exceeds the timestamp range",
            )
        })
    }

    #[cfg(unix)]
    fn mode_of(metadata: &fs::Metadata) -> Option<u32> {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o7777)
    }

    #[cfg(not(unix))]
    fn mode_of(_metadata: &fs::Metadata) -> Option<u32> {
        None
    }
}
