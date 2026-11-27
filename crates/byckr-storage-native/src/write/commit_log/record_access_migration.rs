use super::document_image::DocumentImage;
use crate::{NativeStorageError, NativeStorageErrorCode};

/// What `fylo pack --protect` does with the POSTIX access descriptor
/// (`user.fylo.access`) a record of the source carries (ADR 0021, decision
/// P5-8). A protected database enforces access per application identity,
/// never per record, so a descriptor is either dropped or kept as plain
/// metadata the application may enforce itself; a record's file mode, the
/// descriptor's projection, is dropped either way.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecordAccessMigration {
    /// Refuse a source whose records carry descriptors: neither flag was
    /// given.
    #[default]
    Refuse,
    /// `--drop-record-access`: drop every descriptor.
    Drop,
    /// `--keep-record-access-as-metadata`: keep each descriptor, unchanged
    /// and unenforced, as the metadata field `legacyAccess`.
    KeepAsMetadata,
}

impl RecordAccessMigration {
    const LEGACY_ACCESS: &'static str = "user.fylo.meta.legacyAccess";

    /// The images `images` of `collection` as the protected database holds
    /// them.
    ///
    /// # Errors
    ///
    /// Returns `EPOLICY` for a descriptor under [`Self::Refuse`], or for a
    /// record whose metadata already has a `legacyAccess` field under
    /// [`Self::KeepAsMetadata`], and `CorruptMetadata` for a corrupt
    /// descriptor.
    pub(crate) fn apply(
        self,
        collection: &str,
        images: Vec<DocumentImage>,
    ) -> Result<Vec<DocumentImage>, NativeStorageError> {
        let mut migrated = Vec::with_capacity(images.len());
        for image in images {
            migrated.push(self.migrated(collection, image)?);
        }
        Ok(migrated)
    }

    fn migrated(
        self,
        collection: &str,
        mut image: DocumentImage,
    ) -> Result<DocumentImage, NativeStorageError> {
        image.mode = None;
        crate::decode_access_descriptor(&image.attributes)?;
        let Some(descriptor) = image.attributes.remove(crate::ACCESS_XATTR) else {
            return Ok(image);
        };
        self.place(collection, &mut image, descriptor)?;
        Ok(image)
    }

    fn place(
        self,
        collection: &str,
        image: &mut DocumentImage,
        descriptor: Vec<u8>,
    ) -> Result<(), NativeStorageError> {
        match self {
            Self::Refuse => Err(Self::refused(collection, &image.id)),
            Self::Drop => Ok(()),
            Self::KeepAsMetadata => Self::keep(collection, image, descriptor),
        }
    }

    fn keep(
        collection: &str,
        image: &mut DocumentImage,
        descriptor: Vec<u8>,
    ) -> Result<(), NativeStorageError> {
        let previous = image
            .attributes
            .insert(Self::LEGACY_ACCESS.to_owned(), descriptor);
        if previous.is_none() {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Policy,
            format!(
                "record {} of collection {collection} already has the metadata field \
                 legacyAccess, so its access descriptor cannot be kept there; rename that \
                 field, or pass --drop-record-access",
                image.id
            ),
        ))
    }

    fn refused(collection: &str, id: &str) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::Policy,
            format!(
                "record {id} of collection {collection} carries a POSTIX access descriptor \
                 (uid, gid, mode), which a protected database does not enforce: pass \
                 --drop-record-access to drop such descriptors, or \
                 --keep-record-access-as-metadata to keep each as the metadata field \
                 legacyAccess (fylo access report lists them)"
            ),
        )
    }
}
