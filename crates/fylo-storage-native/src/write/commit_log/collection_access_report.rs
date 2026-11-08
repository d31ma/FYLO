use std::collections::BTreeMap;

use serde::Serialize;

use super::document_image::DocumentImage;
use super::record_access_group::RecordAccessGroup;
use crate::segments::LocationState;
use crate::{NativeCollection, NativeRoot, NativeStorageError};

/// What one collection holds that a move to a protected database changes:
/// its records, the POSTIX access descriptors they carry, and its
/// `$encrypted` fields (ADR 0021 phase 6).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionAccessReport {
    /// The collection's name.
    pub collection: String,
    /// `files` or `segments` (ADR 0015).
    pub layout: String,
    /// Live documents.
    pub documents: usize,
    /// Soft-deleted documents.
    pub deleted: usize,
    /// Records, live and deleted, that carry an access descriptor.
    pub records_with_access: usize,
    /// Those records, grouped by owner, group and mode.
    pub access: Vec<RecordAccessGroup>,
    /// The fields its schema encrypts with `fylo.encryption.key`.
    pub encrypted_fields: Vec<String>,
}

impl CollectionAccessReport {
    const FILES: &'static str = "files";
    const SEGMENTS: &'static str = "segments";

    /// Read every record of `collection`, a collection of `root`.
    ///
    /// # Errors
    ///
    /// Returns an error when a record cannot be read or carries a corrupt
    /// access descriptor.
    pub(crate) fn of(
        root: &NativeRoot,
        collection: &NativeCollection,
    ) -> Result<Self, NativeStorageError> {
        let images = DocumentImage::read_collection(collection)?;
        let groups = Self::groups(&images)?;
        Ok(Self {
            collection: collection.name().to_owned(),
            layout: Self::layout(collection).to_owned(),
            documents: Self::count(&images, LocationState::Live),
            deleted: Self::count(&images, LocationState::Deleted),
            records_with_access: groups.values().sum(),
            access: Self::rendered(groups),
            encrypted_fields: root
                .recorded_encrypted_fields(collection.name())?
                .unwrap_or_default(),
        })
    }

    fn layout(collection: &NativeCollection) -> &'static str {
        if collection.uses_segments() {
            return Self::SEGMENTS;
        }
        Self::FILES
    }

    fn count(images: &[DocumentImage], state: LocationState) -> usize {
        images.iter().filter(|image| image.state == state).count()
    }

    fn groups(
        images: &[DocumentImage],
    ) -> Result<BTreeMap<(u32, u32, u32), usize>, NativeStorageError> {
        let mut groups = BTreeMap::new();
        for image in images {
            let descriptor = crate::decode_access_descriptor(&image.attributes)?;
            Self::add(&mut groups, descriptor);
        }
        Ok(groups)
    }

    fn add(
        groups: &mut BTreeMap<(u32, u32, u32), usize>,
        descriptor: Option<crate::AccessDescriptor>,
    ) {
        let Some(descriptor) = descriptor else {
            return;
        };
        *groups
            .entry((descriptor.uid, descriptor.gid, descriptor.mode))
            .or_default() += 1;
    }

    fn rendered(groups: BTreeMap<(u32, u32, u32), usize>) -> Vec<RecordAccessGroup> {
        let mut rendered = Vec::with_capacity(groups.len());
        for (owner, records) in groups {
            rendered.push(RecordAccessGroup::new(owner, records));
        }
        rendered
    }
}
