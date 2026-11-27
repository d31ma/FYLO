use crate::root_damage::RootDamage;
use serde::Serialize;

/// What `fylo verify --root <root>` found: every damaged file, so an
/// operator can restore them all from one backup (L1 spec 20.2).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootVerificationReport {
    /// The verified root.
    pub root: String,
    /// True when no file is damaged.
    pub ok: bool,
    /// Collections walked.
    pub collections_checked: usize,
    /// Documents and files read and checked.
    pub files_checked: usize,
    /// Every damaged file, with its code.
    pub damaged: Vec<RootDamage>,
}
