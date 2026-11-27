/// Which root the drive rule judges (ADR 0033 section 5.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RootRole {
    /// A repository root, or a root opened on its own: it must sit directly
    /// below a mount point.
    Primary,
    /// A branch worktree inside an admitted repository root: the repository
    /// root carries the mount-point test, so only its disk is judged.
    Worktree,
}
