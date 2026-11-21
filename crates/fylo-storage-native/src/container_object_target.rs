use serde::{Deserialize, Serialize};

/// The record a large object belongs to: its collection and its TTID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerObjectTarget {
    /// The owning collection's id.
    pub collection: u32,
    /// The record's TTID.
    pub id: String,
}
