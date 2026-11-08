use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::extended_attribute_error::ExtendedAttributeError;
use crate::{CollectionKind, FilesystemProfile, NativeRoot, NativeStorageError};

/// A short-lived empty file a native writer uses to check that its root's
/// filesystem keeps extended attributes before it creates a bucket.
///
/// Every raw file of a bucket carries its logical key in `user.fylo.key`, so
/// a bucket on a filesystem without extended attributes (#106) could never
/// hold a file. Documents keep working there until a write adds metadata, so
/// only a bucket is checked, and only on the native profile: the
/// efs-sidecar-v1 profile keeps attributes in sidecars and a `.vdbx`
/// database keeps them in its records.
///
/// The file sits at the top of the root, where no listing looks, and its
/// name ends in `.tmp` like every other scratch file. It is removed when the
/// probe is dropped, so it is gone whether the check passed or failed.
pub(crate) struct ExtendedAttributeProbe {
    path: PathBuf,
    file: File,
}

impl ExtendedAttributeProbe {
    const ATTRIBUTE: &'static str = "user.fylo.probe";
    const VALUE: &'static [u8] = b"1";
    const FILE_PREFIX: &'static str = ".fylo-xattr-probe-";
    const FILE_SUFFIX: &'static str = ".tmp";

    /// Check `root` before a collection of `kind` is created in it, when that
    /// collection is a bucket whose attributes would be extended attributes.
    pub(crate) fn check_before_create(
        root: &NativeRoot,
        kind: CollectionKind,
    ) -> Result<(), NativeStorageError> {
        Self::check_when(Self::needs_extended_attributes(root, kind), root.path())
    }

    fn needs_extended_attributes(root: &NativeRoot, kind: CollectionKind) -> bool {
        kind == CollectionKind::File
            && root.filesystem_profile() == FilesystemProfile::Native
            && !root.is_database()
    }

    fn check_when(needed: bool, directory: &Path) -> Result<(), NativeStorageError> {
        if !needed {
            return Ok(());
        }
        Self::create(directory)?.set_and_remove()
    }

    fn create(directory: &Path) -> Result<Self, NativeStorageError> {
        let path = directory.join(Self::file_name());
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(NativeStorageError::io)?;
        Ok(Self { path, file })
    }

    fn file_name() -> String {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let nanoseconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        format!(
            "{}{}-{nanoseconds}-{}{}",
            Self::FILE_PREFIX,
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
            Self::FILE_SUFFIX
        )
    }

    fn set_and_remove(&self) -> Result<(), NativeStorageError> {
        use xattr::FileExt;

        self.file
            .set_xattr(Self::ATTRIBUTE, Self::VALUE)
            .map_err(|error| self.error(error, "setxattr"))?;
        self.file
            .remove_xattr(Self::ATTRIBUTE)
            .map_err(|error| self.error(error, "removexattr"))
    }

    fn error(&self, error: std::io::Error, operation: &str) -> NativeStorageError {
        ExtendedAttributeError::for_attribute(error, operation, Self::ATTRIBUTE, &self.path)
    }
}

impl Drop for ExtendedAttributeProbe {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
