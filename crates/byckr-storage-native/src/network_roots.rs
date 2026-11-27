/// The switch that admits a root on a network file system through the
/// drive rule (ADR 0033 section 6) instead of refusing it with
/// `EREMOTEROOT` (ADR 0029).
///
/// It stays off until the lease with heartbeat and fencing (slice L1-3) and
/// the file-system probe (slice L1-4) are on `develop`: without them, two
/// machines could each own one root on a share. Those slices turn it on.
pub(crate) struct NetworkRoots;

impl NetworkRoots {
    /// Whether a network root is admitted.
    pub(crate) const ADMITTED: bool = false;
}
