use crate::NativeStorageError;
use crate::layout::RootLayout;
use crate::lease::KernelFileLock;
use fylo_vfs::{File, OpenOptions};
use std::io::ErrorKind;

/// The short app-wide lock `<app>/.fylo/namespaces.lock`, held only while a
/// namespace is created or dropped (ADR 0033 section 3), so two of those
/// never run at the same time in one app folder. The kernel drops it when
/// its process ends. A file system without advisory locks gives no lock to
/// wait for; the namespace lease still keeps a drop away from a live owner.
pub(super) struct NamespacesLock {
    file: File,
}

impl NamespacesLock {
    pub(super) fn take(layout: &RootLayout) -> Result<Self, NativeStorageError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(layout.namespaces_lock_path())
            .map_err(NativeStorageError::io)?;
        Self::lock_unless_unsupported(&file)?;
        Ok(Self { file })
    }

    fn lock_unless_unsupported(file: &File) -> Result<(), NativeStorageError> {
        match KernelFileLock::lock(file) {
            Err(error) if error.kind() != ErrorKind::Unsupported => {
                Err(NativeStorageError::io(error))
            }
            _ => Ok(()),
        }
    }
}

impl Drop for NamespacesLock {
    fn drop(&mut self) {
        let _ = KernelFileLock::unlock(&self.file);
    }
}
