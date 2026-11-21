use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Where one entry lives below the cache folder (ADR 0030 section 4.3):
/// `<root key>/<collection key>/<first 2 hex of the id hash>/<id hash>.fyc`.
///
/// Every part is a hash, so no caller's id or collection name becomes a
/// path. The root key is a hash of the root's canonical path and its
/// device, the collection key a hash of the collection's namespace and
/// name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EntryKey {
    folder: PathBuf,
    name: String,
}

impl EntryKey {
    const ROOT_KEY_BYTES: usize = 16;
    const COLLECTION_KEY_BYTES: usize = 8;
    const NAME_BYTES: usize = 16;
    const SPREAD_HEX: usize = 2;
    const SUFFIX: &'static str = ".fyc";
    const INDEX_NAME: &'static str = "index";

    /// The 32 hex characters that name the root at `canonical` on `device`.
    pub(crate) fn root_key(canonical: &Path, device: u64) -> String {
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_os_str().as_encoded_bytes());
        hasher.update([0_u8]);
        hasher.update(device.to_le_bytes());
        Self::hex(&hasher.finalize()[..Self::ROOT_KEY_BYTES])
    }

    /// The entry of document `identifier` in collection `collection` of the
    /// root `root_key` names.
    pub(crate) fn document(root_key: &str, collection: &str, identifier: &str) -> Self {
        let collection_key = Self::hash_hex(collection, Self::COLLECTION_KEY_BYTES);
        let name = Self::hash_hex(identifier, Self::NAME_BYTES);
        let spread = name[..Self::SPREAD_HEX].to_owned();
        Self {
            folder: [root_key, &collection_key, &spread].iter().collect(),
            name: format!("{name}{}", Self::SUFFIX),
        }
    }

    /// The one index entry of collection `collection` of the root
    /// `root_key` names; a newer stamp replaces it in place.
    pub(crate) fn index(root_key: &str, collection: &str) -> Self {
        let collection_key = Self::hash_hex(collection, Self::COLLECTION_KEY_BYTES);
        Self {
            folder: [root_key, collection_key.as_str()].iter().collect(),
            name: format!("{}{}", Self::INDEX_NAME, Self::SUFFIX),
        }
    }

    /// The folder that holds the entry, below the cache folder.
    pub(crate) fn folder(&self) -> &Path {
        &self.folder
    }

    /// The entry's file name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The entry's path below the cache folder.
    pub(crate) fn path(&self) -> PathBuf {
        self.folder.join(&self.name)
    }

    /// A name, in the entry's folder, for a partial file of this process.
    pub(crate) fn partial_name(&self, suffix: &str) -> String {
        format!("{}.partial-{}-{suffix}", self.name, std::process::id())
    }

    fn hash_hex(text: &str, bytes: usize) -> String {
        Self::hex(&Sha256::digest(text.as_bytes())[..bytes])
    }

    fn hex(bytes: &[u8]) -> String {
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(text, "{byte:02x}");
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::EntryKey;
    use std::path::Path;

    #[test]
    fn names_are_hashes_of_fixed_lengths() {
        let root = EntryKey::root_key(Path::new("/data/root"), 7);
        let key = EntryKey::document(&root, "docs/users", "../../etc/passwd");
        assert_eq!(root.len(), 32);
        let parts: Vec<_> = key.path().iter().map(std::ffi::OsStr::to_owned).collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[1].len(), 16);
        assert_eq!(parts[2].len(), 2);
        assert_eq!(key.name().len(), 32 + ".fyc".len());
        assert!(key.name().starts_with(parts[2].to_str().unwrap()));
    }

    #[test]
    fn another_root_device_or_collection_is_another_entry() {
        let root = EntryKey::root_key(Path::new("/data/root"), 7);
        let key = EntryKey::document(&root, "docs/users", "id");
        let other_device = EntryKey::root_key(Path::new("/data/root"), 8);
        assert_ne!(root, other_device);
        assert_ne!(key, EntryKey::document(&root, "docs/orders", "id"));
        assert_ne!(key, EntryKey::document(&other_device, "docs/users", "id"));
    }

    #[test]
    fn partial_names_are_marked_as_partial() {
        let root = EntryKey::root_key(Path::new("/data/root"), 7);
        let key = EntryKey::document(&root, "docs/users", "id");
        assert!(key.partial_name("1f-3").contains(".partial-"));
    }
}
