use serde::{Deserialize, Serialize};

use super::MetadataStore;

/// The file-system probe result an app folder records in `format.json`
/// (ADR 0033 section 6). Slice L1-4 runs the probe; this type only holds it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeRecord {
    metadata: MetadataStore,
    hard_links: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_attribute_bytes: Option<u32>,
}

impl ProbeRecord {
    /// A probe result.
    #[must_use]
    pub const fn new(metadata: MetadataStore, hard_links: bool) -> Self {
        Self {
            metadata,
            hard_links,
            max_attribute_bytes: None,
        }
    }

    /// The same result with the largest single extended attribute value the
    /// probe could store (FYLO-L1-4b). The probe chooses attributes only when
    /// this holds FYLO's largest metadata value; otherwise the whole app
    /// folder keeps metadata in sidecar files.
    #[must_use]
    pub const fn with_max_attribute_bytes(mut self, bytes: Option<u32>) -> Self {
        self.max_attribute_bytes = bytes;
        self
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

    /// The largest single extended attribute value the probe could store,
    /// measured up to 64 KiB; `None` when extended attributes did not work
    /// at all, or for an app folder made before FYLO-L1-4b.
    #[must_use]
    pub const fn max_attribute_bytes(self) -> Option<u32> {
        self.max_attribute_bytes
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
    fn the_attribute_limit_is_recorded_when_measured() {
        let probe = ProbeRecord::new(MetadataStore::ExtendedAttributes, true)
            .with_max_attribute_bytes(Some(4000));
        assert_eq!(
            serde_json::to_string(&probe).unwrap(),
            r#"{"metadata":"xattr","hardLinks":true,"maxAttributeBytes":4000}"#
        );
    }

    #[test]
    fn a_record_without_the_attribute_limit_still_reads() {
        let text = r#"{"metadata":"xattr","hardLinks":true}"#;
        let probe: ProbeRecord = serde_json::from_str(text).unwrap();
        assert_eq!(probe.max_attribute_bytes(), None);
    }

    #[test]
    fn unknown_fields_are_refused() {
        let text = r#"{"metadata":"xattr","hardLinks":true,"extra":1}"#;
        assert!(serde_json::from_str::<ProbeRecord>(text).is_err());
    }
}
