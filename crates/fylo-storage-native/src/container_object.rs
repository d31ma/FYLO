use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ContainerObjectError, ContainerObjectTarget};

/// A large object stored in a `.vdbx` file: the fields its record carries
/// (ADR 0034 section 1). The record, not the extents, is what makes the
/// object exist.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerObject {
    /// The record the object belongs to.
    #[serde(flatten)]
    pub target: ContainerObjectTarget,
    /// The object's size in bytes.
    pub length: u64,
    /// The SHA-256 of the object's plain bytes, in lowercase hex.
    pub sha256: String,
    /// Plain bytes of a full chunk.
    pub chunk_size: u64,
    /// The object's extents in order, as `[first extent, extent count]`.
    pub runs: Vec<[u64; 2]>,
    /// The 32-byte salt of the object key, in lowercase hex; only in an
    /// encrypted object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salt: Option<String>,
    /// The sequence number of the first extent the write took. It is never
    /// used again, so it tells this write's frames from any later write's.
    pub stamp: u64,
}

impl ContainerObject {
    /// The largest object, in bytes (ADR 0034 section 7).
    pub const MAX_BYTES: u64 = 512 << 20;
    /// The most runs a record may name.
    pub const MAX_RUNS: usize = 4096;
    const FRAME_LABEL: &'static [u8] = b"fylo.vdbx.object.frame.v1";
    const HASH_BYTES: usize = 8;
    const KEY_BYTES: usize = 32;

    /// The hash every frame of this object write carries.
    #[must_use]
    pub fn frame_hash(&self) -> u64 {
        Self::frame_hash_of(&self.target, self.stamp)
    }

    /// The frame hash of the write of `target` that took `stamp`.
    #[must_use]
    pub fn frame_hash_of(target: &ContainerObjectTarget, stamp: u64) -> u64 {
        let digest = Sha256::new()
            .chain_update(Self::FRAME_LABEL)
            .chain_update(target.collection.to_le_bytes())
            .chain_update(stamp.to_le_bytes())
            .chain_update(target.id.as_bytes())
            .finalize();
        let mut first = [0_u8; Self::HASH_BYTES];
        first.copy_from_slice(&digest[..Self::HASH_BYTES]);
        u64::from_le_bytes(first)
    }

    /// Every extent of the object, in object order.
    #[must_use]
    pub fn extents(&self) -> Vec<u64> {
        self.runs
            .iter()
            .flat_map(|[first, count]| *first..first + count)
            .collect()
    }

    /// The extent at position `ordinal` in the object, if it has one.
    #[must_use]
    pub fn extent_at(&self, ordinal: u64) -> Option<u64> {
        let mut left = ordinal;
        for [first, count] in &self.runs {
            if left < *count {
                return Some(first + left);
            }
            left -= count;
        }
        None
    }

    /// The runs that list `extents` in order: neighbours join one run.
    #[must_use]
    pub fn runs_from(extents: &[u64]) -> Vec<[u64; 2]> {
        let mut runs: Vec<[u64; 2]> = Vec::new();
        for extent in extents {
            Self::extend_runs(&mut runs, *extent);
        }
        runs
    }

    /// The salt of an encrypted object.
    ///
    /// # Errors
    ///
    /// Returns `Damaged` when the stored salt is not 32 bytes of hex.
    pub fn salt_bytes(&self) -> Result<Option<[u8; 32]>, ContainerObjectError> {
        self.salt.as_deref().map(Self::parse_key_hex).transpose()
    }

    /// The object's SHA-256.
    ///
    /// # Errors
    ///
    /// Returns `Damaged` when the stored value is not 32 bytes of hex.
    pub fn sha256_bytes(&self) -> Result<[u8; 32], ContainerObjectError> {
        Self::parse_key_hex(&self.sha256)
    }

    /// Lowercase hex of `bytes`.
    #[must_use]
    pub fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn extend_runs(runs: &mut Vec<[u64; 2]>, extent: u64) {
        match runs.last_mut() {
            Some([first, count]) if *first + *count == extent => *count += 1,
            _ => runs.push([extent, 1]),
        }
    }

    fn parse_key_hex(text: &str) -> Result<[u8; 32], ContainerObjectError> {
        let mut bytes = [0_u8; Self::KEY_BYTES];
        let pairs = text.as_bytes().chunks(2);
        if text.len() != 2 * Self::KEY_BYTES {
            return Err(ContainerObjectError::damaged(
                "a stored hash or salt has the wrong length",
            ));
        }
        for (byte, pair) in bytes.iter_mut().zip(pairs) {
            *byte = Self::parse_hex_pair(pair)?;
        }
        Ok(bytes)
    }

    fn parse_hex_pair(pair: &[u8]) -> Result<u8, ContainerObjectError> {
        std::str::from_utf8(pair)
            .ok()
            .and_then(|text| u8::from_str_radix(text, 16).ok())
            .ok_or_else(|| ContainerObjectError::damaged("a stored hash or salt is not hex"))
    }
}
