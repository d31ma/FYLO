use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Canonical document metadata supplied by the storage adapter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalMetadata {
    /// Opaque document identifier.
    pub id: String,
    /// TTID-derived creation time in Unix milliseconds.
    pub created_at: u64,
    /// Last document or metadata update in whole Unix milliseconds.
    pub updated_at: u64,
    /// Native file modification time in whole Unix milliseconds.
    pub mtime: u64,
}

impl CanonicalMetadata {
    /// Merge custom metadata with canonical storage metadata.
    ///
    /// Canonical fields always win, so a custom xattr cannot spoof document
    /// identity or native timestamps.
    #[must_use]
    pub fn merge_with_custom(&self, custom: &Map<String, Value>) -> Map<String, Value> {
        let mut merged = custom.clone();
        merged.insert("id".into(), Value::String(self.id.clone()));
        merged.insert("createdAt".into(), Value::from(self.created_at));
        merged.insert("updatedAt".into(), Value::from(self.updated_at));
        merged.insert("mtime".into(), Value::from(self.mtime));
        merged
    }
}
