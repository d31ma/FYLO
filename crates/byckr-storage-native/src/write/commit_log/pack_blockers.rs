use std::path::Path;

use crate::platform_companion::read_dir_without_companions;
use crate::{FilesystemProfile, NativeRoot, NativeStorageError, path_exists_no_follow};

/// What a directory root uses that a `.vdbx` database does not hold yet
/// (ADR 0016 section 1), so `fylo pack` refuses it.
pub(crate) struct PackBlockers;

impl PackBlockers {
    /// One sentence for each feature of `root` a database does not hold,
    /// in a fixed order; none for a database root.
    ///
    /// # Errors
    ///
    /// Returns an error when one of the root's folders cannot be read.
    pub(crate) fn of(root: &NativeRoot) -> Result<Vec<String>, NativeStorageError> {
        let found = [
            Self::profile(root),
            Self::version_control(root)?,
            Self::queue(root)?,
            Self::bucket(root)?,
        ];
        Ok(found.into_iter().flatten().collect())
    }

    fn profile(root: &NativeRoot) -> Option<String> {
        if root.filesystem_profile() == FilesystemProfile::Native {
            return None;
        }
        Some("cannot pack a root on the EFS sidecar profile, which has no .vdbx form".into())
    }

    fn version_control(root: &NativeRoot) -> Result<Option<String>, NativeStorageError> {
        if !path_exists_no_follow(&root.repository_root().join(".fylo-vcs"))? {
            return Ok(None);
        }
        Ok(Some(
            "cannot pack a root that uses document version control, which a .vdbx database does not hold"
                .into(),
        ))
    }

    fn queue(root: &NativeRoot) -> Result<Option<String>, NativeStorageError> {
        let entry = Self::first_entry(&root.path().join(".fylo-queue"))?;
        Ok(entry.map(|_| {
            "cannot pack a root that uses the embedded queue, which a .vdbx database does not hold"
                .to_owned()
        }))
    }

    fn bucket(root: &NativeRoot) -> Result<Option<String>, NativeStorageError> {
        let entry = Self::first_entry(&root.path().join(".buckets"))?;
        Ok(entry.map(|bucket| {
            format!(
                "cannot pack a root that uses buckets (raw-file collections), which a .vdbx database does not hold: bucket \"{bucket}\""
            )
        }))
    }

    /// The name of one entry of `directory`, or `None` when it is absent or
    /// empty.
    fn first_entry(directory: &Path) -> Result<Option<String>, NativeStorageError> {
        if !path_exists_no_follow(directory)? {
            return Ok(None);
        }
        let mut entries = read_dir_without_companions(directory).map_err(NativeStorageError::io)?;
        entries
            .next()
            .transpose()
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .map_err(NativeStorageError::io)
    }
}
