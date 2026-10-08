use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::ops::Deref;

use fylo_vfs::File;

use crate::sidecar_attributes::SidecarAttributes;
use crate::{FilesystemProfile, NativeRoot, NativeStorageError};

pub(crate) struct RecordAttributes {
    values: BTreeMap<String, Vec<u8>>,
    sidecar: Option<File>,
    sidecar_path: PathBuf,
}

impl RecordAttributes {
    pub(crate) fn read(
        root: &NativeRoot, file: &File, path: &Path,
    ) -> Result<Self, NativeStorageError> {
        root.assert_profile_owned()?;
        let (values, sidecar) = match root.filesystem_profile() {
            FilesystemProfile::Native => (crate::read_fylo_attributes(file, path)?, None),
            FilesystemProfile::EfsSidecarV1 => {
                let (values, sidecar) = SidecarAttributes::read_verified(root, file, path)?;
                (values, Some(sidecar))
            }
        };
        Ok(Self { values, sidecar, sidecar_path: crate::sidecar_path(path) })
    }

    pub(crate) fn verify(
        &self, root: &NativeRoot, payload: &File, path: &Path,
    ) -> Result<(), NativeStorageError> {
        root.assert_profile_owned()?;
        root.verify_open_file_identity(path, payload)?;
        if let Some(sidecar) = self.sidecar.as_ref() {
            root.verify_path(&self.sidecar_path, crate::ExpectedType::File)?;
            root.verify_open_file_identity(&self.sidecar_path, sidecar)?;
            SidecarAttributes::verify_owner(
                root, &sidecar.metadata().map_err(NativeStorageError::io)?, true,
            )?;
        }
        Ok(())
    }

    pub(crate) fn initialize(root: &NativeRoot, path: &Path) -> Result<(), NativeStorageError> {
        if root.filesystem_profile() == FilesystemProfile::EfsSidecarV1 {
            SidecarAttributes::initialize(root, path)?;
        }
        Ok(())
    }

    pub(crate) fn verify_contents(
        &self,
        root: &NativeRoot,
        path: &Path,
        bytes: &[u8],
    ) -> Result<(), NativeStorageError> {
        if root.filesystem_profile() == FilesystemProfile::EfsSidecarV1 {
            SidecarAttributes::verify_bytes_binding(root, path, bytes, &self.values)?;
        }
        Ok(())
    }

    pub(crate) fn write(
        root: &NativeRoot,
        path: &Path,
        name: &str,
        value: &[u8],
    ) -> Result<(), NativeStorageError> {
        match root.filesystem_profile() {
            FilesystemProfile::Native => crate::write::write_fylo_attribute(path, name, value),
            FilesystemProfile::EfsSidecarV1 => {
                Self::require_user_attribute(name)?;
                let mut attributes = SidecarAttributes::read_path(root, path)?;
                attributes.insert(name.into(), value.to_vec());
                SidecarAttributes::replace(root, path, &attributes)?;
                crate::write::failpoint("after-metadata-write")
            }
        }
    }

    pub(crate) fn remove(
        root: &NativeRoot,
        path: &Path,
        name: &str,
    ) -> Result<(), NativeStorageError> {
        match root.filesystem_profile() {
            FilesystemProfile::Native => crate::write::remove_fylo_attribute(path, name),
            FilesystemProfile::EfsSidecarV1 => {
                Self::require_user_attribute(name)?;
                let mut attributes = SidecarAttributes::read_path(root, path)?;
                attributes.remove(name);
                SidecarAttributes::replace(root, path, &attributes)?;
                crate::write::failpoint("after-metadata-write")
            }
        }
    }

    fn require_user_attribute(name: &str) -> Result<(), NativeStorageError> {
        if name == crate::sidecar_attributes::IDENTITY_ATTRIBUTE {
            return Err(NativeStorageError::new(
                crate::NativeStorageErrorCode::Unsupported,
                "record binding is reserved storage metadata",
            ));
        }
        Ok(())
    }
}

impl Deref for RecordAttributes {
    type Target = BTreeMap<String, Vec<u8>>;

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
