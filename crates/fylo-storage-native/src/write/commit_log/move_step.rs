use serde::{Deserialize, Serialize};

/// One record file renamed between the live and retained trees.
///
/// The destination was proven absent when the move was planned, so a
/// destination that exists on replay means the move already happened.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MoveStep {
    pub(crate) from: String,
    pub(crate) to: String,
}
