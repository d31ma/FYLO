use std::io::Read;
use std::path::{Path, PathBuf};

use fylo_vfs::{self as fs, File};
use serde::{Deserialize, Serialize};

use crate::{FilesystemProfile, NativeStorageError, NativeStorageErrorCode, RootLease};

const FORMAT: &str = "fylo.filesystem-profile.v1";

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileManifest {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    format: ProfileFormat,
    profile: ProfileName,
}

// Exact singleton strings are deserialized without accepting unknown profiles.
type ProfileFormat = String;
type ProfileName = String;

impl ProfileManifest {
    fn path(root: &Path) -> PathBuf {
        root.join(".fylo-catalog/filesystem-profile.json")
    }

    pub(crate) fn read(root: &Path) -> Result<Option<Self>, NativeStorageError> {
        let parent = root.join(".fylo-catalog");
        let original_parent = match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => metadata,
            Ok(_) => return Err(unsafe_marker()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(NativeStorageError::io(error)),
        };
        #[cfg(not(unix))]
        let _ = original_parent;
        let path = Self::path(root);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
            Ok(_) => return Err(unsafe_marker()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(NativeStorageError::io(error)),
        };
        if metadata.len() > crate::MAX_DESCRIPTOR_BYTES {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "filesystem profile descriptor exceeds its limit",
            ));
        }
        let mut file = File::open(&path).map_err(NativeStorageError::io)?;
        let opened = file.metadata().map_err(NativeStorageError::io)?;
        if !opened.is_file() {
            return Err(unsafe_marker());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let (uid, gid) = Self::identity(root)?;
            if (metadata.dev(), metadata.ino()) != (opened.dev(), opened.ino())
                || (opened.uid(), opened.gid()) != (uid, gid)
                || opened.mode() & 0o7777 != 0o600
            {
                return Err(unsafe_marker());
            }
        }
        #[cfg(not(target_family = "wasm"))]
        let identity = crate::FileIdentity::from_file(
            file.try_clone().map_err(NativeStorageError::io)?,
        ).map_err(NativeStorageError::io)?;
        let mut bytes = Vec::new();
        (&mut file).take(crate::MAX_DESCRIPTOR_BYTES + 1)
            .read_to_end(&mut bytes).map_err(NativeStorageError::io)?;
        if bytes.len() as u64 > crate::MAX_DESCRIPTOR_BYTES {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "filesystem profile descriptor grew beyond its limit",
            ));
        }
        #[cfg(not(target_family = "wasm"))]
        if identity != crate::FileIdentity::from_path(&path).map_err(NativeStorageError::io)? {
            return Err(unsafe_marker());
        }
        let final_metadata = fs::symlink_metadata(&path).map_err(NativeStorageError::io)?;
        let final_parent = fs::symlink_metadata(&parent).map_err(NativeStorageError::io)?;
        if !final_metadata.is_file() || final_metadata.file_type().is_symlink()
            || !final_parent.is_dir() || final_parent.file_type().is_symlink()
        {
            return Err(unsafe_marker());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let (uid, gid) = Self::identity(root)?;
            if (opened.dev(), opened.ino()) != (final_metadata.dev(), final_metadata.ino())
                || (final_metadata.uid(), final_metadata.gid()) != (uid, gid)
                || final_metadata.mode() & 0o7777 != 0o600
                || (original_parent.dev(), original_parent.ino()) != (final_parent.dev(), final_parent.ino())
                || (final_parent.uid(), final_parent.gid()) != (uid, gid)
                || final_parent.mode() & 0o022 != 0
            {
                return Err(unsafe_marker());
            }
        }
        let descriptor: Self = serde_json::from_slice(&bytes).map_err(|_| corrupt_marker())?;
        if descriptor.format != FORMAT || descriptor.profile != "efs-sidecar-v1" {
            return Err(corrupt_marker());
        }
        descriptor.verify_identity(root)?;
        Ok(Some(descriptor))
    }

    pub(crate) fn admit_selection(
        root: &Path,
        profile: FilesystemProfile,
    ) -> Result<Option<Self>, NativeStorageError> {
        let descriptor = Self::read(root)?;
        match (profile, descriptor) {
            (FilesystemProfile::Native, None) => Ok(None),
            (FilesystemProfile::Native, Some(_)) => Err(NativeStorageError::new(
                NativeStorageErrorCode::Unsupported,
                "marked FYLO root requires its explicit filesystem profile",
            )),
            (FilesystemProfile::EfsSidecarV1, Some(descriptor)) => Ok(Some(descriptor)),
            (FilesystemProfile::EfsSidecarV1, None) => {
                Self::identity(root)?;
                if fs::read_dir(root).map_err(NativeStorageError::io)?.next().is_some() {
                    return Err(NativeStorageError::new(
                        NativeStorageErrorCode::Unsupported,
                        "EFS profile initialization requires a fresh empty root",
                    ));
                }
                Ok(None)
            }
        }
    }

    pub(crate) fn initialize(root: &Path, lease: &RootLease) -> Result<(), NativeStorageError> {
        lease.assert_owned()?;
        if !lease.kernel_enforced() || lease.root() != root {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::RootLeaseLost,
                "EFS profile initialization requires the owning kernel lease",
            ));
        }
        if Self::admit_selection(root, FilesystemProfile::EfsSidecarV1)?.is_some() {
            return Ok(());
        }
        let (uid, gid) = Self::identity(root)?;
        let descriptor = Self {
            uid, gid, format: FORMAT.into(), profile: "efs-sidecar-v1".into(),
        };
        let parent = root.join(".fylo-catalog");
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(&parent).map_err(NativeStorageError::io)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(&parent).map_err(NativeStorageError::io)?;
        crate::write::sync_directory(root)?;
        let bytes = serde_json::to_vec(&descriptor).map_err(|_| corrupt_marker())?;
        crate::write::durable_replace_private(&Self::path(root), &bytes)?;
        lease.assert_owned()
    }

    pub(crate) fn verify_identity(&self, root: &Path) -> Result<(), NativeStorageError> {
        if Self::identity(root)? != (self.uid, self.gid) {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::Unsupported,
                "EFS profile root UID/GID differs from its immutable descriptor",
            ));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn identity(root: &Path) -> Result<(u32, u32), NativeStorageError> {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(root).map_err(NativeStorageError::io)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(unsafe_marker());
        }
        Ok((metadata.uid(), metadata.gid()))
    }

    #[cfg(not(unix))]
    fn identity(_root: &Path) -> Result<(u32, u32), NativeStorageError> {
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "EFS metadata profile requires supported POSIX file identity",
        ))
    }
}

fn corrupt_marker() -> NativeStorageError {
    NativeStorageError::new(
        NativeStorageErrorCode::CorruptMetadata,
        "filesystem profile descriptor is unknown or corrupt",
    )
}

fn unsafe_marker() -> NativeStorageError {
    NativeStorageError::new(
        NativeStorageErrorCode::UnsafePath,
        "filesystem profile descriptor is not a stable regular file",
    )
}
