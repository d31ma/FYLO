use std::io::ErrorKind;
use std::path::Path;

use fylo_vfs as fs;

use crate::{NativeStorageError, NativeStorageErrorCode};

/// Three empty plain files, `.collections`, `.buckets` and `.fylo-catalog`,
/// in every app folder and namespace folder (ADR 0033 section 1). An old FYLO
/// binary expects folders with these names, so it fails on them instead of
/// starting an empty database in a new-layout folder.
pub struct GuardFiles;

impl GuardFiles {
    /// The guard file names.
    pub const NAMES: [&'static str; 3] = [".collections", ".buckets", ".fylo-catalog"];

    /// Make the guard files in `folder`; a guard that is already there as a
    /// plain file is kept.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_WRONG_TYPE` when one of the names is a folder or a
    /// link (an old-layout root), and `ENATIVE_IO` when a file cannot be
    /// made.
    pub fn write(folder: &Path) -> Result<(), NativeStorageError> {
        Self::NAMES
            .iter()
            .try_for_each(|name| Self::write_one(&folder.join(name)))?;
        crate::write::sync_directory(folder)
    }

    /// Whether all three guard files are in `folder` as plain files.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_IO` when a name cannot be checked.
    pub fn are_in_place(folder: &Path) -> Result<bool, NativeStorageError> {
        let found = Self::count_plain_files(folder)?;
        Ok(found == Self::NAMES.len())
    }

    /// Whether `name` is one of the guard file names.
    #[must_use]
    pub fn is_guard_name(name: &str) -> bool {
        Self::NAMES.contains(&name)
    }

    fn count_plain_files(folder: &Path) -> Result<usize, NativeStorageError> {
        let mut found = 0;
        for name in Self::NAMES {
            found += usize::from(Self::is_plain_file(&folder.join(name))?);
        }
        Ok(found)
    }

    fn write_one(path: &Path) -> Result<(), NativeStorageError> {
        let created = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path);
        match created {
            Ok(file) => crate::sync_handle(&file).map_err(NativeStorageError::io),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                Self::require_plain_file(path)
            }
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn require_plain_file(path: &Path) -> Result<(), NativeStorageError> {
        if !Self::is_plain_file(path)? {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::WrongType,
                format!(
                    "{} is not a plain file: this folder holds a FYLO root of the old layout",
                    path.display()
                ),
            ));
        }
        Ok(())
    }

    fn is_plain_file(path: &Path) -> Result<bool, NativeStorageError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }
}
