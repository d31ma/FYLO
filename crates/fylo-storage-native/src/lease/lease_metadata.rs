use crate::{NativeStorageError, NativeStorageErrorCode};
use fylo_vfs::{Metadata, OpenOptions};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LeaseMetadata {
    version: u32,
    root: String,
    owner: String,
    #[serde(rename = "pid")]
    process_id: u32,
    host: String,
    acquired_at: u64,
}

impl LeaseMetadata {
    const MAXIMUM_BYTES: u64 = 4096;
    const CURRENT_VERSION: u32 = 1;

    pub(super) fn new(root: &Path, owner: &str) -> Result<Self, NativeStorageError> {
        Ok(Self {
            version: Self::CURRENT_VERSION,
            root: root.to_string_lossy().into_owned(),
            owner: owner.to_owned(),
            process_id: crate::process_id(),
            host: Self::host_name(),
            acquired_at: crate::write::unix_millis()?,
        })
    }

    pub(super) fn write(&self, path: &Path) -> Result<(), NativeStorageError> {
        let encoded = self.encode()?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(NativeStorageError::io)?;
        file.write_all(&encoded).map_err(NativeStorageError::io)?;
        crate::sync_handle(&file).map_err(NativeStorageError::io)
    }

    pub(super) fn read(path: &Path) -> Option<Self> {
        let metadata = fylo_vfs::symlink_metadata(path).ok()?;
        Self::reject_symlink(&metadata)?;
        Self::require_file(&metadata)?;
        Self::require_bounded_size(&metadata)?;
        serde_json::from_slice(&fylo_vfs::read(path).ok()?).ok()
    }

    pub(super) fn verify_owner(&self, owner: &str) -> Option<()> {
        self.require_current_version()?;
        self.require_matching_owner(owner)
    }

    pub(super) fn describe_owner(metadata_path: &Path, canonical: &Path) -> String {
        Self::read(metadata_path).map_or_else(
            || Self::unknown_owner_message(canonical),
            |metadata| metadata.owner_message(canonical),
        )
    }

    fn encode(&self) -> Result<Vec<u8>, NativeStorageError> {
        serde_json::to_vec(self).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                format!("lease metadata cannot be encoded: {error}"),
            )
        })
    }

    fn reject_symlink(metadata: &Metadata) -> Option<()> {
        if metadata.file_type().is_symlink() {
            return None;
        }
        Some(())
    }

    fn require_file(metadata: &Metadata) -> Option<()> {
        if !metadata.is_file() {
            return None;
        }
        Some(())
    }

    fn require_bounded_size(metadata: &Metadata) -> Option<()> {
        if metadata.len() > Self::MAXIMUM_BYTES {
            return None;
        }
        Some(())
    }

    fn require_current_version(&self) -> Option<()> {
        if self.version != Self::CURRENT_VERSION {
            return None;
        }
        Some(())
    }

    fn require_matching_owner(&self, owner: &str) -> Option<()> {
        if self.owner != owner {
            return None;
        }
        Some(())
    }

    fn unknown_owner_message(canonical: &Path) -> String {
        format!(
            "FYLO root already has a live exclusive owner: {}",
            canonical.to_string_lossy()
        )
    }

    fn owner_message(&self, canonical: &Path) -> String {
        format!(
            "FYLO root already has a live exclusive owner (pid {} on {}): {}",
            self.process_id,
            self.host,
            canonical.to_string_lossy()
        )
    }

    fn host_name() -> String {
        std::env::var("HOSTNAME")
            .ok()
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .unwrap_or_else(|| "unknown".into())
    }
}
