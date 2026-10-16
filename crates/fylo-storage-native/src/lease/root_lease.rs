use super::root_lease_inner::RootLeaseInner;
use crate::NativeStorageError;
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
