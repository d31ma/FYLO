use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fylo_vfs::File;
use serde::de::{MapAccess, Visitor};
#[cfg(unix)]
use sha2::{Digest, Sha256};

use crate::verified_manifest::VerifiedManifest;
use crate::write::CapturedAttribute;
use crate::{ExpectedType, FileStamp, NativeRoot, NativeStorageError, NativeStorageErrorCode};

const MAX_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const IDENTITY_ATTRIBUTE: &str = "user.fylo.record-identity";
const IDENTITY_FORMAT: &str = "fylo.record-identity.v1";

pub(crate) struct SidecarAttributes;

impl SidecarAttributes {
    pub(crate) fn read(
        root: &NativeRoot,
        payload: &File,
        path: &Path,
    ) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        Ok(Self::read_verified(root, payload, path)?.values)
    }

    /// The record's verified manifest, with its open file and the file's
    /// stamp from before it was read.
    pub(crate) fn read_verified(
        root: &NativeRoot,
        payload: &File,
        path: &Path,
    ) -> Result<VerifiedManifest, NativeStorageError> {
        root.assert_profile_owned()?;
        let sidecar = crate::sidecar_path(path);
        if !crate::path_exists_no_follow(&sidecar)? {
            return Err(corrupt());
        }
        let (mut file, metadata) = root.open_file(&sidecar, MAX_BYTES)?;
        let opened = FileStamp::of_file(&file).ok();
        Self::verify_owner(
            root,
            &payload.metadata().map_err(NativeStorageError::io)?,
            false,
        )?;
        Self::verify_owner(root, &metadata, true)?;
        let bytes = crate::read_bounded((&mut file).take(MAX_BYTES + 1), MAX_BYTES)?;
        root.verify_open_file_identity(&sidecar, &file)?;
        root.verify_open_file_identity(path, payload)?;
        root.assert_profile_owned()?;
        let attributes = Self::decode(&bytes)?;
        Self::verify_file_binding(root, payload, path, &attributes)?;
        root.verify_open_file_identity(&sidecar, &file)?;
        root.verify_open_file_identity(path, payload)?;
        root.assert_profile_owned()?;
        Ok(VerifiedManifest {
            values: attributes,
            file,
            opened,
        })
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
        let mut attributes = BTreeMap::new();
        Self::refresh_binding(root, path, &mut attributes)?;
        Self::replace(root, path, &attributes)
    }

    pub(crate) fn replace(
        root: &NativeRoot,
        path: &Path,
        attributes: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), NativeStorageError> {
        root.assert_profile_owned()?;
        let metadata = root.verify_path(path, ExpectedType::File)?;
        Self::verify_owner(root, &metadata, false)?;
        let (payload, _) = root.open_file(path, crate::MAX_RAW_FILE_BYTES)?;
        Self::verify_file_binding(root, &payload, path, attributes)?;
        let sidecar = crate::sidecar_path(path);
        let parent = sidecar.parent().ok_or_else(|| {
            NativeStorageError::new(NativeStorageErrorCode::UnsafePath, "manifest has no parent")
        })?;
        root.verify_path(parent, ExpectedType::Directory)?;
        let bytes = Self::encode(attributes)?;
        // Validate the same names, canonical encodings and limits on writes.
        Self::decode(&bytes)?;
        crate::write::durable_replace_private(&sidecar, &bytes)?;
        root.verify_open_file_identity(path, &payload)?;
        root.assert_profile_owned()
    }

    /// The manifest file's bytes for `attributes`: each value in base64, as
    /// [`Self::decode`] reads them.
    pub(crate) fn encode(
        attributes: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Vec<u8>, NativeStorageError> {
        let encoded: BTreeMap<String, String> = attributes
            .iter()
            .map(|(name, value)| (name.clone(), BASE64.encode(value)))
            .collect();
        serde_json::to_vec(&encoded).map_err(|_| corrupt())
    }

    /// Whether `attributes` bind `bytes` as the document `record` names
    /// (kind folder, collection, id), with no file call: the check a temp
    /// tier hit repeats from what its entry holds (ADR 0030 section 4.4).
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn binds(
        attributes: &BTreeMap<String, Vec<u8>>,
        record: [&str; 3],
        bytes: &[u8],
    ) -> bool {
        let [kind_folder, collection, identifier] = record;
        let checksum = crate::sha256_hex(bytes);
        let expected = [
            IDENTITY_FORMAT,
            kind_folder,
            collection,
            identifier,
            checksum.as_str(),
        ];
        let value = serde_json::to_vec(&expected).unwrap_or_default();
        attributes.get(IDENTITY_ATTRIBUTE).map(Vec::as_slice) == Some(value.as_slice())
    }

    pub(crate) fn refresh_binding(
        root: &NativeRoot,
        path: &Path,
        attributes: &mut BTreeMap<String, Vec<u8>>,
    ) -> Result<(), NativeStorageError> {
        let (payload, metadata) = root.open_file(path, crate::MAX_RAW_FILE_BYTES)?;
        Self::verify_owner(root, &metadata, false)?;
        let checksum = Self::hash_file(&payload, Self::payload_limit(root, path)?)?;
        let binding = Self::binding_value(root, path, &checksum)?;
        root.verify_open_file_identity(path, &payload)?;
        attributes.insert(IDENTITY_ATTRIBUTE.into(), binding);
        root.assert_profile_owned()
    }

    /// Bind `attributes` to the payload bytes about to be written, without
    /// reading the payload file.
    #[allow(dead_code)] // its only caller (record_metadata) is native-only; wasm has none yet
    pub(crate) fn bind_payload_bytes(
        root: &NativeRoot,
        path: &Path,
        bytes: &[u8],
        attributes: &mut BTreeMap<String, Vec<u8>>,
    ) -> Result<(), NativeStorageError> {
        let binding = Self::binding_value(root, path, &crate::sha256_hex(bytes))?;
        attributes.insert(IDENTITY_ATTRIBUTE.into(), binding);
        Ok(())
    }

    pub(crate) fn verify_file_binding(
        root: &NativeRoot,
        payload: &File,
        path: &Path,
        attributes: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), NativeStorageError> {
        let checksum = Self::hash_file(payload, Self::payload_limit(root, path)?)?;
        Self::require_binding(attributes, &Self::binding_value(root, path, &checksum)?)
    }

    pub(crate) fn verify_bytes_binding(
        root: &NativeRoot,
        path: &Path,
        bytes: &[u8],
        attributes: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), NativeStorageError> {
        if bytes.len() as u64 > Self::payload_limit(root, path)? {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "bound payload exceeds its limit",
            ));
        }
        Self::require_binding(
            attributes,
            &Self::binding_value(root, path, &crate::sha256_hex(bytes))?,
        )
    }

    fn payload_limit(root: &NativeRoot, path: &Path) -> Result<u64, NativeStorageError> {
        let identity = Self::record_identity(root, path)?;
        Ok(if identity[0] == ".collections" {
            crate::MAX_DOCUMENT_BYTES
        } else {
            crate::MAX_RAW_FILE_BYTES
        })
    }

    fn require_binding(
        attributes: &BTreeMap<String, Vec<u8>>,
        expected: &[u8],
    ) -> Result<(), NativeStorageError> {
        if attributes.get(IDENTITY_ATTRIBUTE).map(Vec::as_slice) != Some(expected) {
            return Err(corrupt());
        }
        Ok(())
    }

    fn binding_value(
        root: &NativeRoot,
        path: &Path,
        checksum: &str,
    ) -> Result<Vec<u8>, NativeStorageError> {
        let [kind_folder, collection, identifier] = Self::record_identity(root, path)?;
        Self::binding_bytes([&kind_folder, &collection, &identifier], checksum)
    }

    fn binding_bytes(record: [&str; 3], checksum: &str) -> Result<Vec<u8>, NativeStorageError> {
        let [kind_folder, collection, identifier] = record;
        serde_json::to_vec(&[
            IDENTITY_FORMAT,
            kind_folder,
            collection,
            identifier,
            checksum,
        ])
        .map_err(|_| corrupt())
    }

    /// The manifest a write-ahead log step writes for a document: `record`
    /// (kind folder, collection, id) bound to `payload`, with `attributes` in
    /// the encoded form a plan carries (ADR 0013, EFS on the log).
    pub(crate) fn planned_manifest(
        record: [&str; 3],
        payload: &[u8],
        attributes: &[CapturedAttribute],
    ) -> Result<Vec<u8>, NativeStorageError> {
        let binding = Self::binding_bytes(record, &crate::sha256_hex(payload))?;
        let mut encoded: BTreeMap<String, String> = attributes
            .iter()
            .map(|attribute| (attribute.name.clone(), attribute.value.clone()))
            .collect();
        encoded.insert(IDENTITY_ATTRIBUTE.into(), BASE64.encode(binding));
        let bytes = serde_json::to_vec(&encoded).map_err(|_| corrupt())?;
        Self::decode(&bytes)?;
        Ok(bytes)
    }

    /// The attributes of manifest `bytes` without the record binding, in
    /// the encoded form a plan carries.
    pub(crate) fn planned_attributes(
        bytes: &[u8],
    ) -> Result<Vec<CapturedAttribute>, NativeStorageError> {
        Ok(Self::plan_form(Self::decode(bytes)?))
    }

    /// Decoded attributes without the record binding, in the encoded form
    /// a plan carries.
    pub(crate) fn plan_form(attributes: BTreeMap<String, Vec<u8>>) -> Vec<CapturedAttribute> {
        attributes
            .into_iter()
            .filter(|(name, _)| name != IDENTITY_ATTRIBUTE)
            .map(|(name, value)| CapturedAttribute {
                name,
                value: BASE64.encode(value),
            })
            .collect()
    }

    pub(crate) fn record_identity(
        root: &NativeRoot,
        path: &Path,
    ) -> Result<[String; 3], NativeStorageError> {
        let relative = path.strip_prefix(root.path()).map_err(|_| corrupt())?;
        let parts = relative
            .components()
            .map(|part| match part {
                std::path::Component::Normal(name) => name.to_str().ok_or_else(corrupt),
                _ => Err(corrupt()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let [kind_folder, collection, location, shard, filename] = parts.as_slice() else {
            return Err(corrupt());
        };
        Self::require_record_location(kind_folder, location, shard)?;
        crate::validate_collection_name(collection).map_err(|_| corrupt())?;
        let identifier = crate::raw_file_identifier(filename).ok_or_else(corrupt)?;
        crate::validate_ttid_shape(identifier).map_err(|_| corrupt())?;
        Self::require_record_filename(kind_folder, filename, identifier)?;
        root.verify_path(
            &root.path().join(kind_folder).join(collection),
            ExpectedType::Directory,
        )?;
        Ok([
            (*kind_folder).into(),
            (*collection).into(),
            identifier.into(),
        ])
    }

    fn require_record_location(
        kind_folder: &str,
        location: &str,
        shard: &str,
    ) -> Result<(), NativeStorageError> {
        if !matches!(kind_folder, ".collections" | ".buckets")
            || !matches!(location, "docs" | ".deleted")
            || shard.is_empty()
            || shard.len() > crate::MAX_SHARD_WIDTH as usize
            || !shard.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(corrupt());
        }
        Ok(())
    }

    fn require_record_filename(
        kind_folder: &str,
        filename: &str,
        identifier: &str,
    ) -> Result<(), NativeStorageError> {
        if kind_folder == ".collections" {
            if filename != format!("{identifier}.json") {
                return Err(corrupt());
            }
        } else {
            crate::validate_raw_extension(filename, identifier).map_err(|_| corrupt())?;
        }
        Ok(())
    }

    pub(crate) fn is_record_area(root: &NativeRoot, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(root.path()) else {
            return false;
        };
        let parts = relative
            .components()
            .map(|part| part.as_os_str().to_str())
            .collect::<Vec<_>>();
        matches!(
            parts.first().copied().flatten(),
            Some(".collections" | ".buckets")
        ) && matches!(parts.get(2).copied().flatten(), Some("docs" | ".deleted"))
    }

    pub(crate) fn verify_member(root: &NativeRoot, path: &Path) -> Result<(), NativeStorageError> {
        let text = path.to_str().ok_or_else(corrupt)?;
        let payload = Path::new(text.strip_suffix(crate::SIDECAR_SUFFIX).unwrap_or(text));
        if Self::is_record_area(root, payload) {
            Self::record_identity(root, payload)?;
            if !crate::path_exists_no_follow(payload)? {
                return Err(corrupt());
            }
            Self::read_path(root, payload)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn hash_file(file: &File, maximum: u64) -> Result<String, NativeStorageError> {
        use std::os::unix::fs::FileExt;
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; 65_536];
        let mut offset = 0_u64;
        loop {
            let count = file
                .read_at(&mut buffer, offset)
                .map_err(NativeStorageError::io)?;
            if count == 0 {
                return Ok(crate::hex_bytes(&digest.finalize()));
            }
            offset = offset.checked_add(count as u64).ok_or_else(corrupt)?;
            if offset > maximum {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::FileTooLarge,
                    "bound payload exceeds its limit",
                ));
            }
            digest.update(&buffer[..count]);
        }
    }

    #[cfg(not(unix))]
    fn hash_file(_file: &File, _maximum: u64) -> Result<String, NativeStorageError> {
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "bound EFS payloads require offset-preserving POSIX reads",
        ))
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "EFS manifest exceeds 16 MiB",
            ));
        }
        let mut decoder = serde_json::Deserializer::from_slice(bytes);
        let encoded =
            serde::Deserializer::deserialize_map(&mut decoder, Self).map_err(|_| corrupt())?;
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
                if field.is_empty()
                    || field.len() > 64
                    || field.contains('\0')
                    || field.contains('/')
                    || field.contains('\\')
                {
                    return Err(corrupt());
                }
                if value.len() > crate::MAX_META_VALUE_BYTES {
                    return Err(NativeStorageError::new(
                        NativeStorageErrorCode::FileTooLarge,
                        "EFS custom metadata exceeds 60 KiB",
                    ));
                }
            }
            if name == IDENTITY_ATTRIBUTE {
                if value.len() > 512 {
                    return Err(corrupt());
                }
            } else {
                total = total
                    .checked_add(name.len())
                    .and_then(|size| size.checked_add(value.len()))
                    .ok_or_else(corrupt)?;
            }
            if total > crate::MAX_FILE_METADATA_BYTES {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::FileTooLarge,
                    "EFS metadata exceeds aggregate limit",
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
        _root: &NativeRoot,
        _metadata: &fylo_vfs::Metadata,
        _manifest: bool,
    ) -> Result<(), NativeStorageError> {
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "EFS sidecars require POSIX identity",
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
        NativeStorageErrorCode::CorruptMetadata,
        "EFS attribute manifest is corrupt",
    )
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use std::collections::BTreeMap;

    use super::{IDENTITY_ATTRIBUTE, IDENTITY_FORMAT, SidecarAttributes};

    const RECORD: [&str; 3] = [".collections", "notes", "4VRNF52JPCO"];

    fn bound(payload: &[u8]) -> BTreeMap<String, Vec<u8>> {
        let checksum = crate::sha256_hex(payload);
        let [kind_folder, collection, identifier] = RECORD;
        let value = [
            IDENTITY_FORMAT,
            kind_folder,
            collection,
            identifier,
            checksum.as_str(),
        ];
        BTreeMap::from([(
            IDENTITY_ATTRIBUTE.to_owned(),
            serde_json::to_vec(&value).unwrap(),
        )])
    }

    #[test]
    fn a_manifest_binds_the_payload_it_was_written_for() {
        assert!(SidecarAttributes::binds(&bound(b"{}"), RECORD, b"{}"));
    }

    #[test]
    fn a_manifest_does_not_bind_other_bytes() {
        assert!(!SidecarAttributes::binds(
            &bound(b"{}"),
            RECORD,
            b"{\"a\":1}"
        ));
    }

    #[test]
    fn a_manifest_does_not_bind_another_record() {
        let other = [".collections", "notes", "4VRNF52JPCP"];
        assert!(!SidecarAttributes::binds(&bound(b"{}"), other, b"{}"));
    }

    #[test]
    fn a_manifest_without_a_binding_binds_nothing() {
        assert!(!SidecarAttributes::binds(&BTreeMap::new(), RECORD, b"{}"));
    }

    #[test]
    fn an_encoded_manifest_reads_back() {
        let attributes = bound(b"{}");
        let bytes = SidecarAttributes::encode(&attributes).unwrap();
        assert_eq!(SidecarAttributes::decode(&bytes).unwrap(), attributes);
    }
}
