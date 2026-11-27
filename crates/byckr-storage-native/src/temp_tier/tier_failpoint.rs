/// Test-only stops in the temp tier's write path (ADR 0030 slice S3).
///
/// When `YCKR_RUST_TEMP_CACHE_FAILPOINT` names a point, the process aborts
/// there, so a test can prove that a write cut short leaves nothing a later
/// read trusts. These points are separate from the storage crash matrix:
/// the temp tier writes no durable state.
pub(crate) struct TierFailpoint;

impl TierFailpoint {
    const VARIABLE: &'static str = "YCKR_RUST_TEMP_CACHE_FAILPOINT";
    /// After half of a partial file is written.
    pub(crate) const TORN_PARTIAL: &'static str = "temp-entry-torn-partial";
    /// After a partial file is complete, before it is renamed.
    pub(crate) const BEFORE_RENAME: &'static str = "temp-entry-before-rename";

    /// Abort the process when the test variable names `point`.
    pub(crate) fn reach(point: &str) {
        if std::env::var(Self::VARIABLE).ok().as_deref() == Some(point) {
            std::process::abort();
        }
    }
}
