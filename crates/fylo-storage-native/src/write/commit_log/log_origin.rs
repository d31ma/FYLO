use std::path::{Path, PathBuf};

use super::super::NativeWriteRoot;
use crate::lease::WeakRootLease;
use crate::{FilesystemProfile, NativeStorageError, RootOpenOptions};

/// The root and collection a log belongs to, and how its writer opened the
/// root, so a closing process can checkpoint the log through the same
/// profile and lease without this registry holding the root (and its lease)
/// open.
#[derive(Clone, Debug)]
pub(crate) struct LogOrigin {
    root: PathBuf,
    repository: PathBuf,
    collection: String,
    profile: FilesystemProfile,
    lease: Option<WeakRootLease>,
}

impl LogOrigin {
    pub(crate) fn of(writer: &NativeWriteRoot, collection: &str) -> Self {
        Self {
            root: writer.path().to_owned(),
            repository: writer.repository_root().to_owned(),
            collection: collection.to_owned(),
            profile: writer.filesystem_profile(),
            lease: writer.owning_lease().map(|lease| lease.downgrade()),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn collection(&self) -> &str {
        &self.collection
    }

    /// A writer for the root as the log's writer opened it, or `None` when
    /// a profiled root's owning lease is already released: the next owner
    /// recovers the log then.
    pub(crate) fn reopen(&self) -> Result<Option<NativeWriteRoot>, NativeStorageError> {
        if self.profile == FilesystemProfile::Native {
            return NativeWriteRoot::open(&self.root).map(Some);
        }
        self.lease
            .as_ref()
            .and_then(WeakRootLease::upgrade)
            .map(|lease| {
                NativeWriteRoot::open_profiled(
                    RootOpenOptions::new(&self.root)
                        .with_repository(&self.repository)
                        .with_profile(self.profile)
                        .with_lease(lease),
                )
            })
            .transpose()
    }
}
