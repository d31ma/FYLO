use serde::{Deserialize, Serialize};

/// Where an app folder keeps record metadata, as the file-system probe chose
/// it at creation (ADR 0033 section 6).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MetadataStore {
    /// Extended attributes (Windows: NTFS alternate data streams).
    #[serde(rename = "xattr")]
    ExtendedAttributes,
    /// A hidden `.<payload>.fylo-attrs` file beside each payload.
    #[serde(rename = "sidecar")]
    SidecarFiles,
}

#[cfg(test)]
mod tests {
    use super::MetadataStore;

    #[test]
    fn the_json_names_follow_the_storage_contract() {
        assert_eq!(
            serde_json::to_string(&MetadataStore::ExtendedAttributes).unwrap(),
            "\"xattr\""
        );
        assert_eq!(
            serde_json::to_string(&MetadataStore::SidecarFiles).unwrap(),
            "\"sidecar\""
        );
        assert!(serde_json::from_str::<MetadataStore>("\"other\"").is_err());
    }
}
