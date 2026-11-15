#[cfg(not(target_family = "wasm"))]
use crate::system_disk::SystemDiskRule;
use crate::{FilesystemProfile, NativeStorageError, NativeStorageErrorCode, PlacementReport};
#[cfg(not(target_family = "wasm"))]
use crate::{PlacementKind, RootPlacement};
use std::path::Path;

/// The filesystem that holds a root, and the rule that a root never lives on
/// a network filesystem (ADR 0029).
///
/// Locks taken on a network filesystem bind only the processes of one
/// machine, so two machines could each own the same root and write it at
/// once. A native root there is refused before anything is written. The
/// explicit EFS profile (ADR 0010) is the one exception: it is admitted as
/// it always was, and only reported.
///
/// A local root must also be on a disk that is not the system disk, unless
/// the opt-out or app mode admits it (ADR 0032).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootFilesystem {
    name: String,
    remote: bool,
    placement: PlacementReport,
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
        let placement = filesystem.admit_placement(path)?;
        Ok(Self {
            placement,
            ..filesystem
        })
    }

    /// Describe the filesystem that holds `path` and where it lies, as
    /// [`Self::admit`] would judge it, without refusing (ADR 0032 section
    /// 6, for the handshake).
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_IO` when the path's volume cannot be described.
    pub fn describe(path: &Path) -> Result<Self, NativeStorageError> {
        let filesystem = Self::detect(path)?;
        let placement = filesystem.report_placement(path);
        Ok(Self {
            placement,
            ..filesystem
        })
    }

    /// Where the root lies, and why it is admitted (ADR 0032 section 6).
    /// Empty for a filesystem described by [`Self::of`].
    #[must_use]
    pub const fn placement(&self) -> PlacementReport {
        self.placement
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

    /// The system-disk rule, for a local root. A remote root that got here
    /// is admitted by the EFS profile and is never on the system disk.
    #[cfg(not(target_family = "wasm"))]
    fn admit_placement(&self, path: &Path) -> Result<PlacementReport, NativeStorageError> {
        if self.remote {
            return Ok(PlacementReport::admitted(None, PlacementKind::DataDisk));
        }
        let placement = RootPlacement::current()
            .map_err(|reason| SystemDiskRule::setting_error(path, reason))?;
        SystemDiskRule::admit(path, &placement)
    }

    #[cfg(not(target_family = "wasm"))]
    fn report_placement(&self, path: &Path) -> PlacementReport {
        if self.remote {
            return PlacementReport::default();
        }
        RootPlacement::current().map_or_else(
            |_| PlacementReport::default(),
            |placement| SystemDiskRule::report(path, &placement),
        )
    }

    /// The browser and WASI engines have no system disk to protect.
    #[cfg(target_family = "wasm")]
    #[expect(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "the native form can fail, and both forms share one signature"
    )]
    fn admit_placement(&self, _path: &Path) -> Result<PlacementReport, NativeStorageError> {
        Ok(PlacementReport::default())
    }

    #[cfg(target_family = "wasm")]
    #[expect(clippy::unused_self, reason = "both forms share one signature")]
    fn report_placement(&self, _path: &Path) -> PlacementReport {
        PlacementReport::default()
    }

    #[cfg(not(target_family = "wasm"))]
    fn detect(path: &Path) -> Result<Self, NativeStorageError> {
        let info = crate::FilesystemInfo::of_path(path).map_err(NativeStorageError::io)?;
        Ok(Self {
            name: info.name,
            remote: info.remote,
            placement: PlacementReport::default(),
        })
    }

    #[cfg(target_family = "wasm")]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the native form can fail, and both forms share one signature"
    )]
    fn detect(_path: &Path) -> Result<Self, NativeStorageError> {
        Ok(Self {
            name: "unknown".to_owned(),
            remote: false,
            placement: PlacementReport::default(),
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
            placement: crate::PlacementReport::default(),
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
    fn a_local_temporary_folder_is_described_and_named() {
        let filesystem = RootFilesystem::of(&std::env::temp_dir()).unwrap();
        assert!(!filesystem.remote());
        assert!(!filesystem.name().is_empty());
    }
}
