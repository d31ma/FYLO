use std::path::{Path, PathBuf};

use crate::temp_tier::TierLink;
use crate::{DatabaseIdentity, DatabaseKey, FilesystemProfile, RootLease};

/// Additive root admission options, separate from the existing `RootConfig`.
#[derive(Clone, Debug)]
pub struct RootOpenOptions {
    pub(crate) root: PathBuf,
    pub(crate) repository: Option<PathBuf>,
    pub(crate) profile: FilesystemProfile,
    pub(crate) lease: Option<RootLease>,
    pub(crate) database_key: Option<DatabaseKey>,
    pub(crate) identity: Option<DatabaseIdentity>,
    pub(crate) tier: TierLink,
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
            database_key: None,
            identity: None,
            tier: TierLink::default(),
        }
    }

    /// Read the root's files-layout documents through `tier`, the temp-disk
    /// read tier (ADR 0030 section 2). A `.vdbx` database never uses it.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn with_temp_tier(mut self, tier: std::sync::Arc<crate::TempCacheTier>) -> Self {
        self.tier = TierLink::to(tier);
        self
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

    /// Unlock an encrypted `.vdbx` database with `key` (ADR 0016 section 9).
    /// A directory root and a plain database ignore it. Without it an
    /// encrypted database opens locked: it lists no collections, and every
    /// read of one and every write fails with `EKEY`.
    #[must_use]
    pub fn with_database_key(mut self, key: DatabaseKey) -> Self {
        self.database_key = Some(key);
        self
    }

    /// Open a protected `.vdbx` database as `identity` (ADR 0021). A
    /// directory root and a database that is not protected ignore it.
    /// Without it a protected database opens locked: it lists no
    /// collections, and every read of one and every write fails with
    /// `ENOIDENTITY`.
    #[must_use]
    pub fn with_identity(mut self, identity: DatabaseIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// The identity a protected `.vdbx` database is opened with, if any.
    #[must_use]
    pub fn identity(&self) -> Option<&DatabaseIdentity> {
        self.identity.as_ref()
    }

    /// The key an encrypted `.vdbx` database is unlocked with, if any.
    #[must_use]
    pub fn database_key(&self) -> Option<&DatabaseKey> {
        self.database_key.as_ref()
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
