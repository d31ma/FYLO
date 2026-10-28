use serde::{Deserialize, Serialize};

use super::super::CapturedAttribute;
use super::blob_reference::BlobReference;

/// The complete post-image of one record file.
///
/// Every field that is present is applied exactly, so replaying a step that
/// was already applied rewrites the same state.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UpdateStep {
    pub(crate) path: String,
    pub(crate) create: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) blob: Option<BlobReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attributes: Option<Vec<CapturedAttribute>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) gid: Option<u32>,
    pub(crate) modified_ms: u64,
}
