use serde::Serialize;

use super::sweep_report::SweepReport;
use super::temp_cache_mode::TempCacheMode;
use super::temp_cache_reason::TempCacheReason;

/// The temp tier as the handshake reports it in `capabilities.tempCache`
/// (ADR 0030 section 7). `usedBytes` and `entries` are from the last sweep.
/// The counters are for operators; results never depend on them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TempCacheStatus {
    /// `auto` or `on`; the capability is `false` when the mode is `off`.
    pub mode: &'static str,
    /// Whether reads use the tier.
    pub active: bool,
    /// Why the tier is not active; `None` when it is.
    pub reason: Option<&'static str>,
    /// The cache folder.
    pub dir: String,
    /// The byte budget, capped at half the free space once that is known.
    pub budget_bytes: u64,
    /// Bytes of entries at the last sweep.
    pub used_bytes: u64,
    /// Entries at the last sweep.
    pub entries: u64,
    /// Reads answered from the tier.
    pub hits: u64,
    /// Reads the tier could not answer.
    pub misses: u64,
    /// Entries checked again against the root's files.
    pub revalidations: u64,
    /// Entries refused as torn, corrupt or stale, and deleted.
    pub rejected: u64,
    /// Entries written.
    pub writes: u64,
    /// Entries removed by sweeps.
    pub evictions: u64,
    /// Failed cache reads and writes, each answered from the root.
    pub errors: u64,
}

impl TempCacheStatus {
    /// An inactive tier, with no sweep totals yet.
    #[must_use]
    pub fn inactive(
        mode: TempCacheMode,
        dir: String,
        budget_bytes: u64,
        reason: TempCacheReason,
    ) -> Self {
        Self {
            mode: mode.as_str(),
            active: false,
            reason: Some(reason.as_str()),
            dir,
            budget_bytes,
            used_bytes: 0,
            entries: 0,
            hits: 0,
            misses: 0,
            revalidations: 0,
            rejected: 0,
            writes: 0,
            evictions: 0,
            errors: 0,
        }
    }

    /// This status with the totals of `sweep`.
    #[must_use]
    pub fn with_sweep(self, sweep: SweepReport) -> Self {
        Self {
            used_bytes: sweep.used_bytes,
            entries: sweep.entries,
            evictions: self.evictions + sweep.evicted,
            ..self
        }
    }
}
