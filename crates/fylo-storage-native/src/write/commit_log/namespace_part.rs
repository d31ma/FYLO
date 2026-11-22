use serde::{Deserialize, Serialize};

use super::commit_manifest::CommitManifest;

/// One collection's share of a namespace commit record: the commit it
/// makes, the generation it publishes, and where its blobs lie in the
/// record's blob area.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NamespacePart {
    pub(crate) collection: String,
    pub(crate) generation: u64,
    pub(crate) blob_offset: u64,
    pub(crate) blob_length: u64,
    pub(crate) commit: CommitManifest,
}

impl NamespacePart {
    /// The part's own blob area inside the record's blobs.
    pub(crate) fn blobs<'a>(&self, record_blobs: &'a [u8]) -> Option<&'a [u8]> {
        let start = usize::try_from(self.blob_offset).ok()?;
        let length = usize::try_from(self.blob_length).ok()?;
        record_blobs.get(start..start.checked_add(length)?)
    }
}
