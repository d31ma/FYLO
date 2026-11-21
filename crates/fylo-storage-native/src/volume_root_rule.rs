/// The switch that turns on the mount-point rule of ADR 0033 section 5.1:
/// a root's parent folder must be a mount point, or the root is refused
/// with `ENOTVOLUMEROOT`.
///
/// It stays off until slice L1-5 lands the namespaces. Until then Mesail
/// keeps its test roots in `<mount>/slice-<id>/<app>`, one folder below the
/// mount point, and the rule would refuse them. While it is off, only the
/// system-disk rule of ADR 0032 applies. L1-5 turns it on.
pub(crate) struct VolumeRootRule;

impl VolumeRootRule {
    /// Whether the mount-point rule refuses a root.
    pub(crate) const ENFORCED: bool = false;
}
