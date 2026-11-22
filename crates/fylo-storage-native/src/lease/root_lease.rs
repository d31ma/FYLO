use super::lease_timing::LeaseTiming;
use super::root_lease_inner::RootLeaseInner;
use super::root_turn::RootTurn;
use crate::{FilesystemProfile, NativeStorageError, NativeStorageErrorCode, RootFilesystem};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

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

    /// Acquire a root owner, waiting until `wait_until` while another
    /// process owns the root.
    ///
    /// Waiting processes take turns: the one that has waited longest takes
    /// the root before a process that starts later (see `RootTurn`), so a
    /// stream of short-lived owners cannot keep a waiter out until its
    /// deadline. Processes that use [`RootLease::acquire_with_profile`] do
    /// not wait and do not take turns.
    ///
    /// # Errors
    /// Returns `RootLocked` when the root was still owned at `wait_until`,
    /// and any other acquisition error at once.
    pub fn acquire_waiting(
        root: impl AsRef<Path>,
        profile: FilesystemProfile,
        wait_until: Instant,
    ) -> Result<Self, NativeStorageError> {
        let turn = RootTurn::wait(root.as_ref(), profile, wait_until)?;
        let inner = RootLeaseInner::acquire_waiting(root.as_ref(), profile, wait_until);
        drop(turn);
        Ok(Self {
            inner: Arc::new(inner?),
        })
    }

    /// Acquire a root owner with an explicit heartbeat and takeover time
    /// instead of `FYLO_LEASE_HEARTBEAT_SECONDS` and
    /// `FYLO_LEASE_TAKEOVER_SECONDS` (ADR 0033 section 4).
    ///
    /// # Errors
    /// As [`RootLease::acquire_with_profile`]; a takeover from an owner that
    /// may live on another machine first waits the takeover time.
    pub fn acquire_with_timing(
        root: impl AsRef<Path>,
        profile: FilesystemProfile,
        timing: LeaseTiming,
    ) -> Result<Self, NativeStorageError> {
        let inner = RootLeaseInner::acquire_timed(root.as_ref(), profile, timing, None)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// [`RootLease::acquire_waiting`] with an explicit heartbeat and
    /// takeover time.
    ///
    /// # Errors
    /// As [`RootLease::acquire_waiting`].
    pub fn acquire_waiting_with_timing(
        root: impl AsRef<Path>,
        profile: FilesystemProfile,
        timing: LeaseTiming,
        wait_until: Instant,
    ) -> Result<Self, NativeStorageError> {
        let turn = RootTurn::wait(root.as_ref(), profile, wait_until)?;
        let inner =
            RootLeaseInner::acquire_waiting_timed(root.as_ref(), profile, timing, wait_until);
        drop(turn);
        Ok(Self {
            inner: Arc::new(inner?),
        })
    }

    /// Give up the kernel lock but keep the lease record and heartbeat, as
    /// a router reboot does to the locks of an NFS client.
    #[cfg(feature = "test-support")]
    pub fn drop_kernel_lock_for_test(&self) {
        self.inner.drop_kernel_lock_for_test();
    }

    /// Whether the heartbeat has seen another owner's record.
    #[cfg(test)]
    pub(super) fn heartbeat_lost(&self) -> bool {
        self.inner.heartbeat_lost()
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

    /// The filesystem that holds the root, found before the lease was
    /// taken (ADR 0029).
    #[must_use]
    pub fn filesystem(&self) -> &RootFilesystem {
        self.inner.filesystem()
    }

    #[must_use]
    pub fn platform_enforces_exclusivity() -> bool {
        Self::platform_lock_available()
    }

    /// Inside a [`crate::ReadScope`], the owner record is read once per
    /// scope; every later call in the same scope reuses that answer.
    ///
    /// # Errors
    ///
    /// Returns an error if the recorded generation no longer belongs to this lease.
    pub fn assert_owned(&self) -> Result<(), NativeStorageError> {
        let key = self.scope_key();
        self.assert_owned_unless(crate::ReadScope::is_verified(key))?;
        crate::ReadScope::remember_verified(key);
        Ok(())
    }

    /// Read the owner record now, even inside a [`crate::ReadScope`].
    pub(crate) fn assert_owned_fresh(&self) -> Result<(), NativeStorageError> {
        self.inner.assert_owned()
    }

    fn assert_owned_unless(&self, verified: bool) -> Result<(), NativeStorageError> {
        if verified {
            return Ok(());
        }
        self.inner.assert_owned()
    }

    fn scope_key(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&("root-lease", self.inner.owner_generation()), &mut hasher);
        std::hash::Hasher::finish(&hasher)
    }

    /// Finish this unique root owner before exposing an invocation response.
    ///
    /// # Errors
    ///
    /// Rejects surviving lease clones, missing kernel admission, lost ownership,
    /// or failed owner metadata cleanup or unlock.
    pub fn release_checked(self) -> Result<(), NativeStorageError> {
        let inner = Self::take_unique(self.inner)?;
        inner.release_checked()
    }

    fn take_unique(inner: Arc<RootLeaseInner>) -> Result<RootLeaseInner, NativeStorageError> {
        Arc::try_unwrap(inner).map_err(|_| {
            NativeStorageError::new(
                NativeStorageErrorCode::RootLeaseLost,
                "checked root release requires all other lease handles to be dropped",
            )
        })
    }

    fn platform_lock_available() -> bool {
        !cfg!(target_arch = "wasm32")
    }
}
