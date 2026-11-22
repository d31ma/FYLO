use std::sync::{Arc, Weak};

use super::root_lease::RootLease;
use super::root_lease_inner::RootLeaseInner;

/// A root lease named without being kept alive, so a record of which lease
/// wrote a log never stops that lease's checked release.
#[derive(Clone, Debug)]
pub(crate) struct WeakRootLease {
    inner: Weak<RootLeaseInner>,
}

impl WeakRootLease {
    pub(super) fn of(inner: &Arc<RootLeaseInner>) -> Self {
        Self {
            inner: Arc::downgrade(inner),
        }
    }

    /// The lease, while its owner still holds it.
    pub(crate) fn upgrade(&self) -> Option<RootLease> {
        self.inner.upgrade().map(RootLease::from_inner)
    }
}
