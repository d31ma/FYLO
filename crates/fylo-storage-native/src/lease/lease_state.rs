use serde::{Deserialize, Serialize};

/// Whether a lease record's owner still holds it, or stopped cleanly.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum LeaseState {
    Held,
    Released,
}
