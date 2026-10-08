use std::path::{Path, PathBuf};

use crate::{FilesystemProfile, RootLease};

/// Additive root admission options, separate from the existing RootConfig.
#[derive(Clone, Debug)]
pub struct RootOpenOptions {
    pub(crate) root: PathBuf,
    pub(crate) repository: Option<PathBuf>,
    pub(crate) profile: FilesystemProfile,
    pub(crate) lease: Option<RootLease>,
}

impl RootOpenOptions {
    /// Select an existing root using the unchanged native default.
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            repository: None,
            profile: FilesystemProfile::Native,
            lease: None,
        }
    }

    /// Select a metadata representation explicitly.
    #[must_use]
    pub const fn with_profile(mut self, profile: FilesystemProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Attach the canonical repository for a materialized worktree.
    #[must_use]
    pub fn with_repository(mut self, repository: impl AsRef<Path>) -> Self {
        self.repository = Some(repository.as_ref().to_path_buf());
        self
    }

    /// Attach a clone of the owning lease, keeping its kernel handle alive.
    #[must_use]
    pub fn with_lease(mut self, lease: RootLease) -> Self {
        self.lease = Some(lease);
        self
    }

    /// Selected filesystem root before canonicalization.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Optional canonical catalog and repository root.
    #[must_use]
    pub fn repository_root(&self) -> Option<&Path> {
        self.repository.as_deref()
    }

    /// Explicit metadata representation.
    #[must_use]
    pub const fn filesystem_profile(&self) -> FilesystemProfile {
        self.profile
    }

    /// Owning lease clone; this does not create another root owner.
    #[must_use]
    pub fn owning_lease(&self) -> Option<RootLease> {
        self.lease.clone()
    }
}
