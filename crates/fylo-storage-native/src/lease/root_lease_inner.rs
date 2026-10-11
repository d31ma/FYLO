use super::lease_metadata::LeaseMetadata;
use crate::{FilesystemProfile, NativeStorageError, NativeStorageErrorCode};
use fylo_vfs::{File, OpenOptions};
use std::fs::TryLockError;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct RootLeaseInner {
    root: PathBuf,
    owner: String,
    lock_file: File,
    metadata_path: PathBuf,
    kernel_enforced: bool,
}

impl RootLeaseInner {
    pub(super) fn acquire(root: impl AsRef<Path>) -> Result<Self, NativeStorageError> {
        Self::acquire_with_profile(root, FilesystemProfile::Native)
    }

    pub(super) fn acquire_with_profile(
        root: impl AsRef<Path>,
        profile: FilesystemProfile,
    ) -> Result<Self, NativeStorageError> {
        if profile == FilesystemProfile::Native {
            fylo_vfs::create_dir_all(root.as_ref()).map_err(NativeStorageError::io)?;
        } else if !fylo_vfs::metadata(root.as_ref())
            .map_err(NativeStorageError::io)?
            .is_dir()
        {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::WrongType,
                "EFS profile requires an existing directory",
            ));
        }
        let canonical = fylo_vfs::canonicalize(root.as_ref()).map_err(NativeStorageError::io)?;
        crate::profile_manifest::ProfileManifest::admit_selection(&canonical, profile)?;
        let (lock_path, metadata_path) = Self::lease_paths(&canonical)?;
        let lock_file = Self::open_lock_file(&lock_path)?;
        let kernel_enforced =
            Self::lock_root(lock_file.try_lock(), &metadata_path, &canonical, profile)?;
        let owner = crate::write::unique_name("rust-root-owner");
        let metadata = LeaseMetadata::new(&canonical, &owner)?;
        metadata.write(&metadata_path)?;
        Ok(Self {
            root: canonical,
            owner,
            lock_file,
            metadata_path,
            kernel_enforced,
        })
    }

    pub(super) fn owner_generation(&self) -> &str {
        &self.owner
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn kernel_enforced(&self) -> bool {
        self.kernel_enforced
    }

    pub(super) fn assert_owned(&self) -> Result<(), NativeStorageError> {
        let recorded = LeaseMetadata::read(&self.metadata_path);
        let verified = recorded.and_then(|metadata| metadata.verify_owner(&self.owner));
        verified.ok_or_else(Self::ownership_lost)
    }

    pub(super) fn release_checked(self) -> Result<(), NativeStorageError> {
        self.require_kernel_lock()?;
        self.remove_owner_metadata()?;
        self.unlock_checked()
    }

    fn require_kernel_lock(&self) -> Result<(), NativeStorageError> {
        if self.kernel_enforced {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "checked root release requires successful kernel advisory lock admission",
        ))
    }

    fn remove_owner_metadata(&self) -> Result<(), NativeStorageError> {
        self.assert_owned()?;
        fylo_vfs::remove_file(&self.metadata_path).map_err(NativeStorageError::io)?;
        self.sync_metadata_parent()
    }

    fn sync_metadata_parent(&self) -> Result<(), NativeStorageError> {
        let parent = self.metadata_path.parent().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "root owner metadata has no parent directory",
            )
        })?;
        crate::write::sync_directory(parent)
    }

    fn unlock_checked(&self) -> Result<(), NativeStorageError> {
        self.lock_file.unlock().map_err(NativeStorageError::io)
    }

    fn ownership_lost() -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::RootLeaseLost,
            "exclusive ownership of this FYLO root was lost",
        )
    }

    fn lease_paths(canonical: &Path) -> Result<(PathBuf, PathBuf), NativeStorageError> {
        let parent = canonical.parent().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "FYLO root has no parent directory",
            )
        })?;
        let root_name = canonical.file_name().map_or_else(
            || "root".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let lock_path = parent.join(format!(".{root_name}.fylo-root-owner.lock"));
        let metadata_path = parent.join(format!(".{root_name}.fylo-root-owner.lock.json"));
        Ok((lock_path, metadata_path))
    }

    fn open_lock_file(path: &Path) -> Result<File, NativeStorageError> {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(NativeStorageError::io)
    }

    pub(super) fn lock_root(
        attempt: Result<(), TryLockError>,
        metadata_path: &Path,
        canonical: &Path,
        profile: FilesystemProfile,
    ) -> Result<bool, NativeStorageError> {
        match attempt {
            Ok(()) => Ok(true),
            // The lock file is already open read-write, so a refusal here is
            // another holder: SMB (cifs) reports a held byte-range lock as
            // EACCES rather than EWOULDBLOCK.
            Err(TryLockError::WouldBlock) => Err(Self::root_locked(metadata_path, canonical)),
            Err(TryLockError::Error(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                Err(Self::root_locked(metadata_path, canonical))
            }
            Err(TryLockError::Error(error)) if error.kind() == std::io::ErrorKind::Unsupported => {
                if profile == FilesystemProfile::EfsSidecarV1 {
                    Err(NativeStorageError::new(
                        NativeStorageErrorCode::Unsupported,
                        "EFS profile requires supported kernel advisory locking",
                    ))
                } else {
                    Ok(false)
                }
            }
            Err(TryLockError::Error(error)) => Err(NativeStorageError::io(error)),
        }
    }

    fn root_locked(metadata_path: &Path, canonical: &Path) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::RootLocked,
            LeaseMetadata::describe_owner(metadata_path, canonical),
        )
    }
}

impl Drop for RootLeaseInner {
    fn drop(&mut self) {
        let _ = self.lock_file.unlock();
    }
}
