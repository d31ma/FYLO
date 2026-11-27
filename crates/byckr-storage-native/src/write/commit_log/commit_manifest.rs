use serde::{Deserialize, Serialize};

use super::commit_step::CommitStep;

/// The manifest of one commit record.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CommitManifest {
    pub(crate) format: String,
    pub(crate) operation: String,
    pub(crate) steps: Vec<CommitStep>,
    pub(crate) index_removed: Vec<String>,
    pub(crate) index_added: Vec<String>,
}

impl CommitManifest {
    pub(crate) const FORMAT: &'static str = "fylo.log-commit.v1";
}
