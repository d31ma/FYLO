use super::placement_kind::PlacementKind;

/// What the handshake says about where a root lives (ADR 0032 section 6).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlacementReport {
    /// Whether the root is on the system disk. `None` when it was not
    /// looked up: no root, a network root, the opt-out, app mode, or BYCKR
    /// could not tell.
    pub system_disk: Option<bool>,
    /// Why the root is admitted; `None` when it would be refused.
    pub placement: Option<PlacementKind>,
}

impl PlacementReport {
    /// A root that is admitted for `kind`.
    #[must_use]
    pub const fn admitted(system_disk: Option<bool>, kind: PlacementKind) -> Self {
        Self {
            system_disk,
            placement: Some(kind),
        }
    }

    /// A root that would be refused.
    #[must_use]
    pub const fn refused(system_disk: Option<bool>) -> Self {
        Self {
            system_disk,
            placement: None,
        }
    }
}
