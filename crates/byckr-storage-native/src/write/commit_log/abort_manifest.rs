use serde::{Deserialize, Serialize};

/// The manifest of an abort record: one commit a live writer rolled back.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AbortManifest {
    pub(crate) format: String,
    pub(crate) aborts: u64,
}

impl AbortManifest {
    pub(crate) const FORMAT: &'static str = "fylo.log-abort.v1";

    pub(crate) fn new(aborts: u64) -> Self {
        Self {
            format: Self::FORMAT.into(),
            aborts,
        }
    }
}
