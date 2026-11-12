use crate::{FilesystemProfile, NativeStorageError, NativeStorageErrorCode};
use std::path::Path;

/// The filesystem that holds a root, and the rule that a root never lives on
/// a network filesystem (ADR 0029).
///
/// Locks taken on a network filesystem bind only the processes of one
/// machine, so two machines could each own the same root and write it at
/// once. A native root there is refused before anything is written. The
/// explicit EFS profile (ADR 0010) is the one exception: it is admitted as
/// it always was, and only reported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootFilesystem {
    name: String,
    remote: bool,
}

impl RootFilesystem {
    /// Describe the filesystem that holds `path`, or would hold it once
    /// created. Nothing is written.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_IO` when the path's volume cannot be described.
    pub fn of(path: &Path) -> Result<Self, NativeStorageError> {
        Self::detect(path)
    }

    /// Describe the filesystem that holds `path`, and refuse it with
    /// `EREMOTEROOT` when it is a network filesystem and `profile` is the
    /// native one.
    ///
    /// # Errors
    ///
    /// Returns `EREMOTEROOT` for a native root on a network filesystem, or
    /// `ENATIVE_IO` when the path's volume cannot be described.
    pub fn admit(path: &Path, profile: FilesystemProfile) -> Result<Self, NativeStorageError> {
        let filesystem = Self::detect(path)?;
        filesystem.refuse_remote(path, profile)?;
        Ok(filesystem)
    }

    /// The name the OS reports, such as `apfs`, `ext4`, `NTFS` or `nfs`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the root is reached over a network or served by another
    /// kernel.
    #[must_use]
    pub const fn remote(&self) -> bool {
        self.remote
    }

    fn refuse_remote(
        &self,
        path: &Path,
        profile: FilesystemProfile,
    ) -> Result<(), NativeStorageError> {
        if !self.remote || profile == FilesystemProfile::EfsSidecarV1 {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::RemoteRoot,
            format!(
                "FYLO root {} is on a network file system ({}). File locks there do not stop \
                 another machine from writing the same root, so FYLO does not use it. Use a \
                 disk attached to this machine.",
                path.display(),
                self.name
            ),
        ))
    }

    #[cfg(not(target_family = "wasm"))]
    fn detect(path: &Path) -> Result<Self, NativeStorageError> {
        let info = crate::FilesystemInfo::of_path(path).map_err(NativeStorageError::io)?;
        Ok(Self {
            name: info.name,
            remote: info.remote,
        })
    }

    #[cfg(target_family = "wasm")]
    fn detect(_path: &Path) -> Result<Self, NativeStorageError> {
        Ok(Self {
            name: "unknown".to_owned(),
            remote: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RootFilesystem;
    use crate::{FilesystemProfile, NativeStorageErrorCode};
    use std::path::Path;

    fn remote() -> RootFilesystem {
        RootFilesystem {
            name: "nfs".to_owned(),
            remote: true,
        }
    }

    #[test]
    fn a_native_root_on_a_network_filesystem_is_refused_with_its_name_and_path() {
        let error = remote()
            .refuse_remote(Path::new("/mnt/share/root"), FilesystemProfile::Native)
            .unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::RemoteRoot);
        assert_eq!(error.code().as_str(), "EREMOTEROOT");
        let message = error.to_string();
        assert!(message.contains("/mnt/share/root"), "{message}");
        assert!(message.contains("(nfs)"), "{message}");
        assert!(
            message.contains("disk attached to this machine"),
            "{message}"
        );
    }

    #[test]
    fn the_efs_profile_is_admitted_on_a_network_filesystem() {
        remote()
            .refuse_remote(Path::new("/mnt/efs/root"), FilesystemProfile::EfsSidecarV1)
            .unwrap();
    }

    #[test]
    fn a_local_temporary_folder_is_admitted_and_named() {
        let filesystem =
            RootFilesystem::admit(&std::env::temp_dir(), FilesystemProfile::Native).unwrap();
        assert!(!filesystem.remote());
        assert!(!filesystem.name().is_empty());
    }
}
