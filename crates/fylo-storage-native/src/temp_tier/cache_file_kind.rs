/// What a file in a temp cache folder is, from its name (ADR 0030
/// section 4.3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheFileKind {
    /// A finished entry, `<name>.fyc`.
    Entry,
    /// An entry still being written, or left by a crash:
    /// `<name>.partial-<pid>-<random>`.
    Partial,
    /// Anything else, such as `sweep.stamp`. A sweep leaves it alone.
    Other,
}
