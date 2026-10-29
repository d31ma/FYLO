use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{LocationState, SegmentBody};

/// The complete post-image of one document in a segment record.
///
/// `attributes` maps each FYLO extended attribute name to its base64 value,
/// exactly what the file layout keeps in `user.fylo.*` attributes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SegmentEntry {
    pub(crate) id: String,
    pub(crate) state: LocationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body: Option<SegmentBody>,
    #[serde(default)]
    pub(crate) attributes: BTreeMap<String, String>,
    pub(crate) modified_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<u32>,
}
