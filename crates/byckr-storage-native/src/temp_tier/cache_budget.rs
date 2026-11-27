/// The temp tier's byte budget (ADR 0030 section 4.5).
pub struct CacheBudget;

impl CacheBudget {
    /// The budget when none is set: 256 MiB.
    pub const DEFAULT_BYTES: u64 = 256 * 1024 * 1024;
    /// The free space below which `auto` stays off: 64 MiB.
    pub const MINIMUM_FREE_BYTES: u64 = 64 * 1024 * 1024;
    const FREE_SHARE_DIVISOR: u64 = 2;

    /// The `requested` budget, or the default, never more than half of
    /// `free_bytes`, the free space of the cache folder's filesystem.
    #[must_use]
    pub fn cap(requested: Option<u64>, free_bytes: u64) -> u64 {
        Self::wanted(requested).min(free_bytes / Self::FREE_SHARE_DIVISOR)
    }

    /// The `requested` budget, or the default, before the cap.
    #[must_use]
    pub fn wanted(requested: Option<u64>) -> u64 {
        requested.unwrap_or(Self::DEFAULT_BYTES)
    }

    /// Whether `free_bytes` is too little for the tier.
    #[must_use]
    pub const fn is_low(free_bytes: u64) -> bool {
        free_bytes < Self::MINIMUM_FREE_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::CacheBudget;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn the_default_budget_is_256_mib() {
        assert_eq!(CacheBudget::cap(None, 100 * GIB), 256 * 1024 * 1024);
    }

    #[test]
    fn a_budget_never_takes_more_than_half_the_free_space() {
        assert_eq!(CacheBudget::cap(None, 300 * 1024 * 1024), 150 * 1024 * 1024);
        assert_eq!(CacheBudget::cap(Some(10 * GIB), 4 * GIB), 2 * GIB);
    }

    #[test]
    fn a_smaller_requested_budget_is_kept() {
        assert_eq!(CacheBudget::cap(Some(4096), 100 * GIB), 4096);
    }

    #[test]
    fn less_than_64_mib_free_is_low() {
        assert!(CacheBudget::is_low(64 * 1024 * 1024 - 1));
        assert!(!CacheBudget::is_low(64 * 1024 * 1024));
    }
}
