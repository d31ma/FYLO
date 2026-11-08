use std::fmt::Write;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

pub(in crate::schema) struct SchemaFiles;

impl SchemaFiles {
    const MAXIMUM_BYTES: u64 = 16_777_216;
    const HASH_TEXT_LENGTH: usize = 64;
    const NOT_REGULAR: &str = "schema path is not a regular file";

    pub(in crate::schema) fn read(path: &Path) -> Result<Vec<u8>, String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("cannot inspect schema file: {error}"))?;
        Self::reject_symlink(&metadata)?;
        Self::require_file(&metadata)?;
        Self::check_size(metadata.len())?;
        fs::read(path).map_err(|error| format!("cannot read schema file: {error}"))
    }

    pub(in crate::schema) fn hash(path: &Path) -> Result<String, String> {
        let bytes = Self::read(path)?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        Ok(Self::encode_hash(&hasher.finalize()))
    }

    fn reject_symlink(metadata: &fs::Metadata) -> Result<(), String> {
        if metadata.file_type().is_symlink() {
            return Err(Self::NOT_REGULAR.into());
        }
        Ok(())
    }

    fn require_file(metadata: &fs::Metadata) -> Result<(), String> {
        if !metadata.is_file() {
            return Err(Self::NOT_REGULAR.into());
        }
        Ok(())
    }

    fn check_size(length: u64) -> Result<(), String> {
        if length > Self::MAXIMUM_BYTES {
            return Err(format!("schema file exceeds {} bytes", Self::MAXIMUM_BYTES));
        }
        Ok(())
    }

    fn encode_hash(bytes: &[u8]) -> String {
        let mut encoded = String::with_capacity(Self::HASH_TEXT_LENGTH);
        for byte in bytes {
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    }
}
