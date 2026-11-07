use fylo_identity::IdentityId;

/// How a process decides that a protected database's policy is the one it
/// expects (ADR 0021, Q7): the identity that signed policy version 1 must
/// be the pinned anchor, or, only when asked for, whoever it is the first
/// time, with a warning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccessTrust {
    /// The admin identity the database must have been created by.
    Pinned(IdentityId),
    /// Trust the creator found, whoever it is. The open reports it so the
    /// caller can warn and pin it.
    FirstUse,
    /// No anchor given: a protected database refuses to open (`EPOLICY`).
    Unpinned,
}
