use serde::{Deserialize, Serialize};

use super::MetadataStore;

/// The file-system probe result an app folder records in `format.json`
/// (ADR 0033 section 6). Slice L1-4 runs the probe; this type only holds it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeRecord {
    metadata: MetadataStore,
    hard_links: bool,
}

impl ProbeRecord {
    /// A probe result.
    #[must_use]
    pub const fn new(metadata: MetadataStore, hard_links: bool) -> Self {
        Self {
            metadata,
            hard_links,
        }
    }

    /// Where record metadata is kept.
    #[must_use]
    pub const fn metadata(self) -> MetadataStore {
        self.metadata
    }

    /// Whether hard links work; `false` means before-images are copied.
    #[must_use]
    pub const fn hard_links(self) -> bool {
        self.hard_links
    }
}

#[cfg(test)]
mod tests {
    use super::ProbeRecord;
    use crate::layout::MetadataStore;

    #[test]
    fn the_json_follows_the_storage_contract() {
        let probe = ProbeRecord::new(MetadataStore::SidecarFiles, false);
        assert_eq!(
            serde_json::to_string(&probe).unwrap(),
            r#"{"metadata":"sidecar","hardLinks":false}"#
        );
        assert_eq!(probe.metadata(), MetadataStore::SidecarFiles);
        assert!(!probe.hard_links());
    }

    #[test]
    fn unknown_fields_are_refused() {
        let text = r#"{"metadata":"xattr","hardLinks":true,"extra":1}"#;
        assert!(serde_json::from_str::<ProbeRecord>(text).is_err());
    }
}
