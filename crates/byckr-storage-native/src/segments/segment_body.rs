use serde::{Deserialize, Serialize};

/// Where an entry's document bytes sit in its record's blob area.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SegmentBody {
    pub(crate) offset: u64,
    pub(crate) length: u64,
    pub(crate) sha256: String,
}
