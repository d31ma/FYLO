use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fylo_vfs::File;
use serde::de::{MapAccess, Visitor};

use crate::{ExpectedType, NativeRoot, NativeStorageError, NativeStorageErrorCode};

const MAX_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) struct SidecarAttributes;

impl SidecarAttributes {
    pub(crate) fn read(
        root: &NativeRoot,
        payload: &File,
        path: &Path,
    ) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        Ok(Self::read_verified(root, payload, path)?.0)
    }

    pub(crate) fn read_verified(
        root: &NativeRoot, payload: &File, path: &Path,
    ) -> Result<(BTreeMap<String, Vec<u8>>, File), NativeStorageError> {
        root.assert_profile_owned()?;
        let sidecar = crate::sidecar_path(path);
        let (mut file, metadata) = root.open_file(&sidecar, MAX_BYTES)?;
        Self::verify_owner(root, &payload.metadata().map_err(NativeStorageError::io)?, false)?;
        Self::verify_owner(root, &metadata, true)?;
        let bytes = crate::read_bounded((&mut file).take(MAX_BYTES + 1), MAX_BYTES)?;
        root.verify_open_file_identity(&sidecar, &file)?;
        root.verify_open_file_identity(path, payload)?;
        root.assert_profile_owned()?;
        Ok((Self::decode(&bytes)?, file))
    }

    pub(crate) fn read_path(
        root: &NativeRoot,
        path: &Path,
    ) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        let (file, _) = root.open_file(path, crate::MAX_RAW_FILE_BYTES)?;
        Self::read(root, &file, path)
    }

    pub(crate) fn initialize(root: &NativeRoot, path: &Path) -> Result<(), NativeStorageError> {
        let sidecar = crate::sidecar_path(path);
        if crate::path_exists_no_follow(&sidecar)? {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                "new EFS record has an orphan or existing manifest",
            ));
        }
        Self::replace(root, path, &BTreeMap::new())
    }

    pub(crate) fn replace(
        root: &NativeRoot,
        path: &Path,
        attributes: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), NativeStorageError> {
        root.assert_profile_owned()?;
        let metadata = root.verify_path(path, ExpectedType::File)?;
        Self::verify_owner(root, &metadata, false)?;
        let sidecar = crate::sidecar_path(path);
        let parent = sidecar.parent().ok_or_else(|| NativeStorageError::new(
            NativeStorageErrorCode::UnsafePath, "manifest has no parent",
        ))?;
        root.verify_path(parent, ExpectedType::Directory)?;
        let encoded: BTreeMap<String, String> = attributes.iter()
            .map(|(name, value)| (name.clone(), BASE64.encode(value))).collect();
        let bytes = serde_json::to_vec(&encoded).map_err(|_| corrupt())?;
        // Validate the same names, canonical encodings and limits on writes.
        Self::decode(&bytes)?;
        crate::write::durable_replace_private(&sidecar, &bytes)?;
        root.assert_profile_owned()
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge, "EFS manifest exceeds 16 MiB",
            ));
        }
        let mut decoder = serde_json::Deserializer::from_slice(bytes);
        let encoded = serde::Deserializer::deserialize_map(&mut decoder, Self)
            .map_err(|_| corrupt())?;
        decoder.end().map_err(|_| corrupt())?;
        let mut total = 0usize;
        let mut attributes = BTreeMap::new();
        for (name, encoded) in encoded {
            if !name.starts_with("user.fylo.") {
                return Err(corrupt());
            }
            let value = BASE64.decode(&encoded).map_err(|_| corrupt())?;
            if BASE64.encode(&value) != encoded {
                return Err(corrupt());
            }
            if let Some(field) = name.strip_prefix(crate::META_XATTR_PREFIX) {
                if field.is_empty() || field.len() > 64 || field.contains('\0')
                    || field.contains('/') || field.contains('\\')
                {
                    return Err(corrupt());
                }
                if value.len() > crate::MAX_META_VALUE_BYTES {
                    return Err(NativeStorageError::new(
                        NativeStorageErrorCode::FileTooLarge, "EFS custom metadata exceeds 60 KiB",
                    ));
                }
            }
            total = total.checked_add(name.len()).and_then(|size| size.checked_add(value.len()))
                .ok_or_else(corrupt)?;
            if total > crate::MAX_FILE_METADATA_BYTES {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::FileTooLarge, "EFS metadata exceeds aggregate limit",
                ));
            }
            attributes.insert(name, value);
        }
        Ok(attributes)
    }

    #[cfg(unix)]
    pub(crate) fn verify_owner(
        root: &NativeRoot,
        metadata: &fylo_vfs::Metadata,
        manifest: bool,
    ) -> Result<(), NativeStorageError> {
        use std::os::unix::fs::MetadataExt;
        let identity = root.efs_identity().ok_or_else(corrupt)?;
        if (metadata.uid(), metadata.gid()) != identity
            || (manifest && metadata.mode() & 0o7777 != 0o600)
        {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::PermissionDenied,
                "EFS payload or private manifest differs from the immutable root identity",
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    pub(crate) fn verify_owner(
        _root: &NativeRoot, _metadata: &fylo_vfs::Metadata, _manifest: bool,
    ) -> Result<(), NativeStorageError> {
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported, "EFS sidecars require POSIX identity",
        ))
    }
}

impl<'de> Visitor<'de> for SidecarAttributes {
    type Value = BTreeMap<String, String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an attribute map without duplicate names")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut output = BTreeMap::new();
        while let Some((name, value)) = map.next_entry::<String, String>()? {
            if output.insert(name, value).is_some() {
                return Err(serde::de::Error::custom("duplicate EFS attribute name"));
            }
        }
        Ok(output)
    }
}

fn corrupt() -> NativeStorageError {
    NativeStorageError::new(
        NativeStorageErrorCode::CorruptMetadata, "EFS attribute manifest is corrupt",
    )
}
