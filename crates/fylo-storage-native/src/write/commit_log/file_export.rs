use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::super::{
    CapturedAttribute, NativeWriteRoot, durable_replace, ensure_directory, sync_directory,
};
use super::blob_reference::BlobReference;
use super::commit_step::CommitStep;
use super::document_image::DocumentImage;
use super::segment_import::SegmentImport;
use super::step_applier::StepApplier;
use super::update_step::UpdateStep;
use crate::segments::LocationState;
use crate::{NativeRoot, NativeStorageError};

/// Writes document images as the per-document file layout: one JSON file
/// per document with its attributes, mode, owner and modification time, and
/// a prefix index rebuilt from them (ADR 0015 section 9).
pub(crate) struct FileExport;

impl FileExport {
    pub(crate) fn build(
        root: &NativeRoot,
        directory: &Path,
        images: &[DocumentImage],
        shard_width: u32,
    ) -> Result<(), NativeStorageError> {
        ensure_directory(root, directory)?;
        for namespace in ["docs", ".deleted", "index"] {
            ensure_directory(root, &directory.join(namespace))?;
        }
        let applier = StepApplier::live(root, directory);
        let mut written = Vec::with_capacity(images.len());
        let mut keys = BTreeSet::new();
        for image in images {
            let relative = Self::relative_path(image, shard_width);
            applier.apply(
                &CommitStep::Update(Self::step(image, &relative)?),
                &image.body,
            )?;
            written.push(directory.join(&relative));
            if image.state == LocationState::Live {
                keys.extend(NativeWriteRoot::document_keys(&image.id, &image.body)?);
            }
        }
        Self::write_index(directory, keys)?;
        Self::flush(directory, &written)
    }

    /// Where the file layout keeps `image`, relative to its collection.
    pub(crate) fn relative_path(image: &DocumentImage, shard_width: u32) -> String {
        let namespace = match image.state {
            LocationState::Deleted => ".deleted",
            LocationState::Live | LocationState::Absent => "docs",
        };
        format!(
            "{namespace}/{}/{}.json",
            crate::shard_of(&image.id, shard_width),
            image.id
        )
    }

    fn step(image: &DocumentImage, relative: &str) -> Result<UpdateStep, NativeStorageError> {
        let access = crate::decode_access_descriptor(&image.attributes)?;
        Ok(UpdateStep {
            path: relative.to_owned(),
            create: true,
            blob: Some(BlobReference {
                offset: 0,
                length: image.body.len() as u64,
                sha256: crate::sha256_hex(&image.body),
            }),
            attributes: Some(
                SegmentImport::encoded_attributes(&image.attributes)
                    .into_iter()
                    .map(|(name, value)| CapturedAttribute { name, value })
                    .collect(),
            ),
            mode: image.mode,
            uid: access.map(|descriptor| descriptor.uid),
            gid: access.map(|descriptor| descriptor.gid),
            modified_ms: image.modified_ms,
        })
    }

    fn write_index(directory: &Path, keys: BTreeSet<String>) -> Result<(), NativeStorageError> {
        let index = directory.join("index");
        let mut snapshot = Vec::new();
        for key in keys {
            snapshot.extend_from_slice(key.as_bytes());
            snapshot.push(b'\n');
        }
        durable_replace(
            &index.join("manifest.json"),
            format!(
                "{{\"format\":\"fylo.local-fs.index.v1\",\"createdAt\":{}}}\n",
                super::super::now_millis()?
            )
            .as_bytes(),
        )?;
        durable_replace(&index.join("keys.snapshot"), &snapshot)?;
        durable_replace(&index.join("keys.wal"), b"")
    }

    /// Make every written file and directory durable before the copy can be
    /// adopted.
    fn flush(directory: &Path, written: &[PathBuf]) -> Result<(), NativeStorageError> {
        let mut directories = BTreeSet::new();
        for path in written {
            let file = Self::open_for_flush(path)?;
            crate::sync_handle(&file).map_err(NativeStorageError::io)?;
            if let Some(parent) = path.parent() {
                directories.insert(parent.to_owned());
            }
        }
        for namespace in ["docs", ".deleted", "index"] {
            directories.insert(directory.join(namespace));
        }
        directories.insert(directory.to_owned());
        for path in directories {
            sync_directory(&path)?;
        }
        Ok(())
    }

    #[cfg(windows)]
    fn open_for_flush(path: &Path) -> Result<fylo_vfs::File, NativeStorageError> {
        fylo_vfs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(NativeStorageError::io)
    }

    #[cfg(not(windows))]
    fn open_for_flush(path: &Path) -> Result<fylo_vfs::File, NativeStorageError> {
        fylo_vfs::File::open(path).map_err(NativeStorageError::io)
    }
}
