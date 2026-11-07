use serde::{Deserialize, Serialize};

/// A database key wrapped for one identity key (HPKE, RFC 9180): the
/// catalog key, or one epoch of a collection's key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessKeyWrap {
    /// The identifier of the key it is wrapped for.
    pub recipient: String,
    /// The collection's id, or none for the catalog key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<u32>,
    /// The collection key's epoch; none for the catalog key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u32>,
    /// The wrap, base64.
    pub wrap: String,
}
