use std::sync::Arc;

use super::temp_cache_status::TempCacheStatus;
use super::temp_cache_tier::TempCacheTier;

/// The temp tier of one session, decided when it opens its root (ADR 0030
/// section 4.1).
#[derive(Clone, Debug)]
pub enum TempCacheState {
    /// `--temp-cache off`: the capability is `false`.
    Off,
    /// The tier is not used; the status says why.
    Inactive(TempCacheStatus),
    /// Reads go through this tier.
    Active(Arc<TempCacheTier>),
}

impl TempCacheState {
    /// The status the handshake reports, with the tier's counters now;
    /// `None` when the mode is `off`.
    #[must_use]
    pub fn status(&self) -> Option<TempCacheStatus> {
        match self {
            Self::Off => None,
            Self::Inactive(status) => Some(status.clone()),
            Self::Active(tier) => Some(tier.status()),
        }
    }

    /// The tier reads go through, when it is active.
    #[must_use]
    pub fn tier(&self) -> Option<Arc<TempCacheTier>> {
        match self {
            Self::Active(tier) => Some(Arc::clone(tier)),
            Self::Off | Self::Inactive(_) => None,
        }
    }
}
