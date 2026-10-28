use serde::{Deserialize, Serialize};

/// Where one step's bytes sit in its record's blob area, with their digest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BlobReference {
    pub(crate) offset: u64,
    pub(crate) length: u64,
    pub(crate) sha256: String,
}
