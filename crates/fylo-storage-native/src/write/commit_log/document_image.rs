use std::collections::BTreeMap;
use std::path::Path;

use fylo_vfs::{self as fs, File};

use super::record_image::RecordImage;
use crate::segments::{LocationState, SegmentDocument, SegmentStore};
use crate::{NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Everything that makes up one stored document, in a form both layouts
/// can produce and compare: the migration oracle (ADR 0015 section 9).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DocumentImage {
    pub(crate) id: String,
    pub(crate) state: LocationState,
    pub(crate) body: Vec<u8>,
    pub(crate) attributes: BTreeMap<String, Vec<u8>>,
    pub(crate) modified_ms: u64,
    pub(crate) mode: Option<u32>,
}

impl DocumentImage {
    const BATCH: usize = 256;

    /// Every live and deleted document of a collection, in identifier order
    /// within each state.
    pub(crate) fn read_collection(
        collection: &NativeCollection,
    ) -> Result<Vec<Self>, NativeStorageError> {
        if collection.uses_segments() {
            return Self::read_segments(&collection.path);
        }
        let mut images = Vec::new();
        for id in collection.document_ids()? {
            let stored = collection.read_document(&id)?;
            images.push(Self::read_file(&stored.path, (&id, LocationState::Live))?);
        }
        for id in collection.deleted_document_ids()? {
            let stored = collection.read_deleted_document(&id)?;
            images.push(Self::read_file(
                &stored.path,
                (&id, LocationState::Deleted),
            )?);
        }
        Ok(images)
    }

    /// Every live and deleted document of the segment layout in `directory`.
    pub(crate) fn read_segments(directory: &Path) -> Result<Vec<Self>, NativeStorageError> {
        let store = SegmentStore::open_for_writer(directory)?;
        let mut images = Vec::new();
        for state in [LocationState::Live, LocationState::Deleted] {
            for batch in store.ids(state).chunks(Self::BATCH) {
                for document in store.documents(batch, state)? {
                    images.push(Self::from_segment(document)?);
                }
            }
        }
        Ok(images)
    }

    /// One document as the file layout stores it at `path`.
    pub(crate) fn read_file(
        path: &Path,
        (id, state): (&str, LocationState),
    ) -> Result<Self, NativeStorageError> {
        let file = File::open(path).map_err(NativeStorageError::io)?;
        let metadata = fs::symlink_metadata(path).map_err(NativeStorageError::io)?;
        Ok(Self {
            id: id.to_owned(),
            state,
            body: fs::read(path).map_err(NativeStorageError::io)?,
            attributes: crate::read_fylo_attributes(&file, path)?,
            modified_ms: RecordImage::modified_ms_of(&metadata)?,
            mode: Self::mode_of(&metadata),
        })
    }

    pub(crate) fn from_segment(document: SegmentDocument) -> Result<Self, NativeStorageError> {
        Ok(Self {
            attributes: document.attributes()?,
            id: document.entry.id,
            state: document.entry.state,
            modified_ms: document.entry.modified_ms,
            mode: document.entry.mode,
            body: document.body,
        })
    }

    /// Fail unless `copy` holds exactly the documents of `source`. A mode is
    /// compared only where the source records one: a document a segment
    /// collection created without access control has none, and its exported
    /// file takes the process's default.
    pub(crate) fn require_same(source: &[Self], copy: &[Self]) -> Result<(), NativeStorageError> {
        let copies: BTreeMap<(&str, u8), &Self> = copy
            .iter()
            .map(|image| ((image.id.as_str(), image.state.code()), image))
            .collect();
        if copies.len() != source.len() || copy.len() != source.len() {
            return Err(Self::mismatch(
                "the copy holds a different set of documents",
            ));
        }
        for image in source {
            let Some(counterpart) = copies.get(&(image.id.as_str(), image.state.code())) else {
                return Err(Self::mismatch(&format!(
                    "document {} is missing from the copy",
                    image.id
                )));
            };
            if !image.matches(counterpart) {
                return Err(Self::mismatch(&format!(
                    "document {} differs in the copy",
                    image.id
                )));
            }
        }
        Ok(())
    }

    fn matches(&self, copy: &Self) -> bool {
        self.body == copy.body
            && self.attributes == copy.attributes
            && self.modified_ms == copy.modified_ms
            && (self.mode.is_none() || self.mode == copy.mode)
    }

    fn mismatch(detail: &str) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::CorruptMetadata,
            format!("layout copy failed verification: {detail}"),
        )
    }

    // The signature mirrors the platforms without POSIX modes.
    #[allow(clippy::unnecessary_wraps)]
    #[cfg(unix)]
    fn mode_of(metadata: &fs::Metadata) -> Option<u32> {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o7777)
    }

    #[cfg(not(unix))]
    fn mode_of(_metadata: &fs::Metadata) -> Option<u32> {
        None
    }
}
