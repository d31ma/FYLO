use serde::{Deserialize, Serialize};

use super::move_step::MoveStep;
use super::update_step::UpdateStep;

/// One file-level change inside a committed mutation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum CommitStep {
    Update(UpdateStep),
    Move(MoveStep),
}
