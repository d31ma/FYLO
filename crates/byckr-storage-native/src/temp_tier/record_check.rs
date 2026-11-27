use byckr_vfs::File;

use super::manifest_seen::ManifestSeen;
use crate::FileStamp;
use crate::record_attributes::RecordAttributes;
use crate::sidecar_attributes::SidecarAttributes;

/// What one root read of a document saw of its files, for the temp tier
/// (ADR 0030 sections 3.2 and 4.4): the payload's stamp once the read
/// passed its checks and, in the EFS profile, the manifest's stamps when it
/// was opened and once it was checked, with the verified manifest's bytes.
pub(crate) struct RecordCheck {
    payload: Option<FileStamp>,
    manifest: Option<ManifestSeen>,
}

impl RecordCheck {
    /// What the read of `payload`, with its verified `attributes`, saw.
    pub(crate) fn of(payload: &File, attributes: &RecordAttributes) -> Self {
        Self {
            payload: FileStamp::of_file(payload).ok(),
            manifest: attributes.sidecar().map(|file| ManifestSeen {
                opened: attributes.sidecar_opened(),
                checked: FileStamp::of_file(file).ok(),
                bytes: SidecarAttributes::encode(attributes).ok(),
            }),
        }
    }

    /// The digest of the record's file stamps when no file changed while it
    /// was read: the payload had the stamp `payload_opened` when it was
    /// opened and the manifest the stamp it had when it was opened.
    pub(crate) fn unchanged_digest(&self, payload_opened: Option<FileStamp>) -> Option<[u8; 32]> {
        let payload = self.payload.filter(|now| payload_opened == Some(*now))?;
        self.digest_with_manifest(payload)
    }

    /// The manifest bytes an entry keeps: empty in the native profile, and
    /// `None` when the manifest could not be encoded.
    pub(crate) fn manifest_bytes(self) -> Option<Vec<u8>> {
        self.manifest
            .map_or(Some(Vec::new()), |manifest| manifest.bytes)
    }

    fn digest_with_manifest(&self, payload: FileStamp) -> Option<[u8; 32]> {
        let Some(manifest) = &self.manifest else {
            return Some(payload.digest_with(None));
        };
        let checked = manifest
            .checked
            .filter(|now| manifest.opened == Some(*now))?;
        Some(payload.digest_with(Some(&checked)))
    }
}
