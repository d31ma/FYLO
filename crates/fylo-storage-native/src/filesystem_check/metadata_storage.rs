use serde::{Deserialize, Serialize};

use crate::FilesystemProfile;

/// Where an app folder keeps the metadata of its payloads, as recorded in
/// `format.json` (`probe.metadata`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MetadataStorage {
    /// Extended attributes (Windows: NTFS alternate data streams).
    #[serde(rename = "xattr")]
    ExtendedAttributes,
    /// Hidden `.fylo-attrs` sidecar files beside each payload (ADR 0010).
    #[serde(rename = "sidecar")]
    SidecarFiles,
}

impl MetadataStorage {
    /// The name `format.json` stores: `xattr` or `sidecar`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        Self::name_of(self)
    }

    /// The engine profile that reads and writes metadata this way.
    #[must_use]
    pub const fn profile(self) -> FilesystemProfile {
        Self::profile_of(self)
    }

    const fn name_of(storage: Self) -> &'static str {
        match storage {
            Self::ExtendedAttributes => "xattr",
            Self::SidecarFiles => "sidecar",
        }
    }

    const fn profile_of(storage: Self) -> FilesystemProfile {
        match storage {
            Self::ExtendedAttributes => FilesystemProfile::Native,
            Self::SidecarFiles => FilesystemProfile::EfsSidecarV1,
        }
    }
}
