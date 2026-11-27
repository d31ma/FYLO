use std::sync::atomic::{AtomicU64, Ordering};

/// The temp tier's counters for one process (ADR 0030 section 7). They are
/// for operators; no result depends on them.
#[derive(Debug, Default)]
pub(crate) struct TierCounters {
    pub(crate) hits: AtomicU64,
    pub(crate) misses: AtomicU64,
    pub(crate) revalidations: AtomicU64,
    pub(crate) rejected: AtomicU64,
    pub(crate) writes: AtomicU64,
    pub(crate) evictions: AtomicU64,
    pub(crate) errors: AtomicU64,
}

impl TierCounters {
    /// Add one to `counter`.
    pub(crate) fn count(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Add `amount` to `counter`.
    pub(crate) fn add(counter: &AtomicU64, amount: u64) {
        counter.fetch_add(amount, Ordering::Relaxed);
    }

    /// The value of `counter` now.
    pub(crate) fn read(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}
