/// Crash points of the lease protocol for the two-process crash matrix
/// (`tests/lease_two_process.rs`): when `YCKR_RUST_FAILPOINT` names one of
/// them, the process aborts there, as a power cut would stop it.
///
/// They are kept apart from the write failpoints (`crate::FAILPOINTS`),
/// whose matrix reaches every name through a document write.
pub(super) struct LeaseFailpoint;

impl LeaseFailpoint {
    const VARIABLE: &str = "YCKR_RUST_FAILPOINT";
    const NAMES: [&str; 3] = [
        "lease-after-claim",
        "lease-after-record",
        "lease-after-release",
    ];

    pub(super) fn reach(name: &str) {
        debug_assert!(
            Self::NAMES.contains(&name),
            "undeclared lease failpoint: {name}"
        );
        let chosen = std::env::var(Self::VARIABLE).is_ok_and(|value| value == name);
        Self::abort_when(chosen);
    }

    fn abort_when(chosen: bool) {
        if chosen {
            std::process::abort();
        }
    }
}
