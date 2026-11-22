use std::path::Path;

use super::super::CapturedAttribute;
use super::blob_reference::BlobReference;
use super::update_step::UpdateStep;
use crate::sidecar_attributes::SidecarAttributes;
use crate::{NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// The `.fylo-attrs` file of a document on the EFS profile, as a log step
/// writes it: the record's attributes and the binding of its payload, built
/// from the planned bytes, so no step reads the payload back (ADR 0010,
/// ADR 0013).
pub(crate) struct SidecarFile;

impl SidecarFile {
    /// The mode of a sidecar file and of a new EFS payload, as the journal
    /// path writes them.
    pub(crate) const PRIVATE_MODE: u32 = 0o600;
    const DOCUMENT_KIND_FOLDER: &'static str = ".collections";

    /// The manifest bytes that bind `payload` as the document at `path`.
    pub(crate) fn manifest(
        collection: &NativeCollection,
        path: &Path,
        payload: &[u8],
        attributes: &[CapturedAttribute],
    ) -> Result<Vec<u8>, NativeStorageError> {
        let identifier = Self::identifier(path)?;
        SidecarAttributes::planned_manifest(
            [Self::DOCUMENT_KIND_FOLDER, collection.name(), identifier],
            payload,
            attributes,
        )
    }

    /// The attributes a manifest holds, without its binding.
    pub(crate) fn attributes(bytes: &[u8]) -> Result<Vec<CapturedAttribute>, NativeStorageError> {
        SidecarAttributes::planned_attributes(bytes)
    }

    /// The step that writes a manifest from `blob`.
    pub(crate) fn step(
        blob: BlobReference,
        create: bool,
    ) -> Result<UpdateStep, NativeStorageError> {
        Ok(UpdateStep {
            path: String::new(),
            create,
            blob: Some(blob),
            attributes: None,
            mode: Some(Self::PRIVATE_MODE),
            uid: None,
            gid: None,
            modified_ms: super::super::now_millis()?,
        })
    }

    /// A new or moved record must not meet an orphan manifest.
    pub(crate) fn require_absent(sidecar: &Path) -> Result<(), NativeStorageError> {
        if crate::path_exists_no_follow(sidecar)? {
            return Err(Self::corrupt(
                "EFS pair destination or orphan manifest already exists",
            ));
        }
        Ok(())
    }

    /// A record that moves must move with its manifest.
    pub(crate) fn require_present(sidecar: &Path) -> Result<(), NativeStorageError> {
        if !crate::path_exists_no_follow(sidecar)? {
            return Err(Self::corrupt("EFS record has no manifest to move with it"));
        }
        Ok(())
    }

    fn corrupt(message: &str) -> NativeStorageError {
        NativeStorageError::new(NativeStorageErrorCode::CorruptMetadata, message)
    }

    fn identifier(path: &Path) -> Result<&str, NativeStorageError> {
        let identifier = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(crate::raw_file_identifier);
        identifier.ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "a planned EFS record has no document identifier",
            )
        })
    }
}
