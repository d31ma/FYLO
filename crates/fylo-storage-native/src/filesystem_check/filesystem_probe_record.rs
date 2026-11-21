use serde::{Deserialize, Serialize};

use crate::{FilesystemProfile, MetadataStorage};

/// The probe result an app folder records in `format.json` (the `probe`
/// object of `api/storage/v2/format.schema.json`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesystemProbeRecord {
    metadata: MetadataStorage,
    hard_links: bool,
}

impl FilesystemProbeRecord {
    /// A record of where metadata lives and whether hard links work.
    #[must_use]
    pub const fn new(metadata: MetadataStorage, hard_links: bool) -> Self {
        Self {
            metadata,
            hard_links,
        }
    }

    /// Where the app folder keeps metadata.
    #[must_use]
    pub const fn metadata(&self) -> MetadataStorage {
        self.metadata
    }

    /// Whether transaction before-images may be hard links; when `false`
    /// they are copied.
    #[must_use]
    pub const fn hard_links(&self) -> bool {
        self.hard_links
    }

    /// The engine profile that matches the recorded metadata storage.
    #[must_use]
    pub const fn profile(&self) -> FilesystemProfile {
        self.metadata.profile()
    }
}
