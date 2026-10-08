use super::root_lease_inner::RootLeaseInner;
use crate::{FilesystemProfile, NativeStorageError};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct RootLease {
    inner: Arc<RootLeaseInner>,
}

impl RootLease {
    /// # Errors
    ///
    /// Returns an error if the root cannot be initialized, locked, or recorded.
    pub fn acquire(root: impl AsRef<Path>) -> Result<Self, NativeStorageError> {
        let inner = RootLeaseInner::acquire(root)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Acquire a root owner using explicit profile admission.
    ///
    /// # Errors
    /// Rejects profile mismatch or unsupported EFS locking before owner publication.
    pub fn acquire_with_profile(
        root: impl AsRef<Path>,
        profile: FilesystemProfile,
    ) -> Result<Self, NativeStorageError> {
        Ok(Self {
            inner: Arc::new(RootLeaseInner::acquire_with_profile(root, profile)?),
        })
    }

    pub(crate) fn owner_generation(&self) -> &str {
        self.inner.owner_generation()
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        self.inner.root()
    }

    #[must_use]
    pub fn kernel_enforced(&self) -> bool {
        self.inner.kernel_enforced()
    }

    #[must_use]
    pub fn platform_enforces_exclusivity() -> bool {
        Self::platform_lock_available()
    }

    /// # Errors
    ///
    /// Returns an error if the recorded generation no longer belongs to this lease.
    pub fn assert_owned(&self) -> Result<(), NativeStorageError> {
        self.inner.assert_owned()
    }

    fn platform_lock_available() -> bool {
        !cfg!(target_arch = "wasm32")
    }
}
