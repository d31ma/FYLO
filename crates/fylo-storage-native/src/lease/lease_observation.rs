use super::lease_record::LeaseRecord;
use fylo_vfs::Metadata;
use std::path::Path;

/// What a process found in a lease record at one moment: the exact bytes,
/// to tell later whether anything changed, and what they mean.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LeaseObservation {
    bytes: Option<Vec<u8>>,
}

impl LeaseObservation {
    const LEGACY_VERSION: u64 = 1;

    pub(super) fn read(path: &Path) -> Self {
        Self {
            bytes: Self::read_bytes(path),
        }
    }

    pub(super) fn record(&self) -> Option<LeaseRecord> {
        LeaseRecord::decode(self.bytes.as_deref()?)
    }

    /// No record, a released one, or one from a binary before the layout
    /// release: the next owner claims at once. Those binaries refused
    /// network roots (ADR 0029), so their owner could only have been kept
    /// out by the kernel lock this process now holds.
    pub(super) fn free_now(&self) -> bool {
        let released = self.record().is_some_and(|record| record.is_released());
        released || self.bytes.is_none() || self.is_legacy()
    }

    /// The record names a process on this machine that has ended.
    pub(super) fn owner_died_here(&self) -> bool {
        self.record()
            .is_some_and(|record| record.owner_died_here())
    }

    /// The generation the next claim follows. An unreadable record gives
    /// none, and the claim files then tell the last generation.
    pub(super) fn generation(&self) -> Option<u64> {
        self.record().map(|record| record.generation())
    }

    pub(super) fn recorded_takeover_millis(&self) -> u64 {
        self.record().map_or(0, |record| record.takeover_millis())
    }

    pub(super) fn owner_message(&self, canonical: &Path) -> String {
        self.record().map_or_else(
            || Self::unknown_owner_message(canonical),
            |record| record.owner_message(canonical),
        )
    }

    fn is_legacy(&self) -> bool {
        let Some(bytes) = self.bytes.as_deref() else {
            return false;
        };
        let value = serde_json::from_slice::<serde_json::Value>(bytes).ok();
        let version = value.as_ref().and_then(|value| value.get("version"));
        version.and_then(serde_json::Value::as_u64) == Some(Self::LEGACY_VERSION)
    }

    /// `None` only when no record exists. A link, a folder, an oversized
    /// file or a failed read gives empty bytes: an unreadable record, never
    /// followed and never mistaken for a missing one.
    fn read_bytes(path: &Path) -> Option<Vec<u8>> {
        let metadata = Self::existing_metadata(path)?;
        let usable = metadata.is_ok_and(|metadata| Self::plain_small_file(&metadata));
        Some(Self::contents_when(usable, path))
    }

    fn existing_metadata(path: &Path) -> Option<std::io::Result<Metadata>> {
        let metadata = fylo_vfs::symlink_metadata(path);
        let missing = metadata
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
        (!missing).then_some(metadata)
    }

    fn contents_when(usable: bool, path: &Path) -> Vec<u8> {
        if !usable {
            return Vec::new();
        }
        fylo_vfs::read(path).unwrap_or_default()
    }

    fn plain_small_file(metadata: &Metadata) -> bool {
        let plain = metadata.is_file() && !metadata.file_type().is_symlink();
        plain && metadata.len() <= LeaseRecord::MAXIMUM_BYTES
    }

    fn unknown_owner_message(canonical: &Path) -> String {
        format!(
            "FYLO root already has a live exclusive owner: {}",
            canonical.to_string_lossy()
        )
    }
}
