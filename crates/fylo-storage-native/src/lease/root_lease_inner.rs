use super::lease_metadata::LeaseMetadata;
use crate::{NativeStorageError, NativeStorageErrorCode};
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
        fylo_vfs::create_dir_all(root.as_ref()).map_err(NativeStorageError::io)?;
        let canonical = fylo_vfs::canonicalize(root.as_ref()).map_err(NativeStorageError::io)?;
        let (lock_path, metadata_path) = Self::lease_paths(&canonical)?;
        let lock_file = Self::open_lock_file(&lock_path)?;
        let kernel_enforced = Self::lock_root(&lock_file, &metadata_path, &canonical)?;
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

    fn lock_root(
        lock_file: &File,
        metadata_path: &Path,
        canonical: &Path,
    ) -> Result<bool, NativeStorageError> {
        match lock_file.try_lock() {
            Ok(()) => Ok(true),
            Err(TryLockError::WouldBlock) => Err(NativeStorageError::new(
                NativeStorageErrorCode::RootLocked,
                LeaseMetadata::describe_owner(metadata_path, canonical),
            )),
            Err(TryLockError::Error(error)) if error.kind() == std::io::ErrorKind::Unsupported => {
                Ok(false)
            }
            Err(TryLockError::Error(error)) => Err(NativeStorageError::io(error)),
        }
    }
}

impl Drop for RootLeaseInner {
    fn drop(&mut self) {
        let _ = self.lock_file.unlock();
    }
}
