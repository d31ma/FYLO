use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::Path;

use super::{MetadataPlan, MetadataPlanInput, SidecarImage};
use crate::sidecar_attributes::SidecarAttributes;
use crate::{NativeRoot, NativeStorageError};

/// Plans the metadata of a record kept in its `.fylo-attrs` sidecar file
/// (ADR 0010).
pub(super) struct SidecarPlan;

impl SidecarPlan {
    pub(super) fn post_image(
        root: &NativeRoot,
        path: &Path,
        input: &MetadataPlanInput<'_>,
    ) -> Result<MetadataPlan, NativeStorageError> {
        let current = Self::current_values(path, input.current_sidecar())?;
        let mut next = input.changes().applied_to(current);
        Self::bind(root, path, input.payload(), &mut next)?;
        let bytes = SidecarAttributes::encode(&next)?;
        SidecarAttributes::decode(&bytes)?;
        Ok(MetadataPlan::new(None, SidecarImage::Replace(bytes)))
    }

    fn current_values(
        path: &Path,
        current_sidecar: Option<&[u8]>,
    ) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        match current_sidecar {
            Some(bytes) => SidecarAttributes::decode(bytes),
            None => Self::read_current(path),
        }
    }

    fn read_current(path: &Path) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        match std::fs::read(crate::sidecar_path(path)) {
            Ok(bytes) => SidecarAttributes::decode(&bytes),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn bind(
        root: &NativeRoot,
        path: &Path,
        payload: Option<&[u8]>,
        values: &mut BTreeMap<String, Vec<u8>>,
    ) -> Result<(), NativeStorageError> {
        match payload {
            Some(bytes) => SidecarAttributes::bind_payload_bytes(root, path, bytes, values),
            None => SidecarAttributes::refresh_binding(root, path, values),
        }
    }
}
