use std::io::ErrorKind;
use std::path::Path;

use super::{CapturedAttribute, MetadataPlan, SidecarImage};
use crate::{NativeRoot, NativeStorageError};

/// The only writer of record metadata. It writes a [`MetadataPlan`]'s final
/// state; writing the same plan again gives the same result, so recovery
/// can redo it.
pub(crate) struct MetadataWriter;

impl MetadataWriter {
    pub(crate) fn apply(
        root: &NativeRoot,
        path: &Path,
        plan: &MetadataPlan,
    ) -> Result<(), NativeStorageError> {
        root.assert_profile_owned()?;
        Self::apply_attributes(path, plan.xattrs())?;
        Self::apply_sidecar(path, plan.sidecar())?;
        root.assert_profile_owned()
    }

    fn apply_attributes(
        path: &Path,
        xattrs: Option<&[CapturedAttribute]>,
    ) -> Result<(), NativeStorageError> {
        match xattrs {
            Some(attributes) => crate::write::restore_attributes(path, attributes),
            None => Ok(()),
        }
    }

    fn apply_sidecar(path: &Path, image: &SidecarImage) -> Result<(), NativeStorageError> {
        match image {
            SidecarImage::Keep => Ok(()),
            SidecarImage::Replace(bytes) => {
                crate::write::durable_replace_private(&crate::sidecar_path(path), bytes)
            }
            SidecarImage::Remove => Self::remove_sidecar(path),
        }
    }

    fn remove_sidecar(path: &Path) -> Result<(), NativeStorageError> {
        match std::fs::remove_file(crate::sidecar_path(path)) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(NativeStorageError::io(error)),
            _ => Ok(()),
        }
    }
}
