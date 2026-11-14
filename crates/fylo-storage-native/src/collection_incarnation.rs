use fylo_format::Crc32c;
use std::path::Path;

use crate::NativeStorageError;

/// Names one life of a files-layout collection (ADR 0030 section 3.3).
///
/// It is a TTID clock tick written to `incarnation` beside the
/// collection's `state.json` when the collection is created. A collection
/// dropped and created again under the same name gets a later tick, so a
/// cache entry of the old life never matches the new one, even when the new
/// `state.json` happens to look like the old one. Older releases ignore the
/// file; a missing, short or damaged file reads as [`Self::UNKNOWN`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CollectionIncarnation(u64);

impl CollectionIncarnation {
    /// The incarnation of a collection that has none recorded.
    pub const UNKNOWN: Self = Self(0);
    pub(crate) const FILE_NAME: &'static str = "incarnation";
    const ENCODED_BYTES: usize = 16;
    const MAGIC: [u8; 4] = *b"FYLN";
    const CHECKED_BYTES: usize = 12;

    /// The incarnation of a collection created now.
    pub(crate) fn fresh() -> Result<Self, NativeStorageError> {
        crate::write::next_ttid_tick().map(Self)
    }

    /// The incarnation recorded at `path`, or [`Self::UNKNOWN`] when the file
    /// is missing, unreadable, short or fails its checksum.
    pub(crate) fn read(path: &Path) -> Self {
        let bytes = fylo_vfs::read(path).unwrap_or_default();
        Self::decode(&bytes)
    }

    /// Whether this names a real incarnation.
    #[must_use]
    pub const fn is_known(self) -> bool {
        self.0 != Self::UNKNOWN.0
    }

    pub(crate) fn encode(self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        bytes[0..4].copy_from_slice(&Self::MAGIC);
        bytes[4..12].copy_from_slice(&self.0.to_le_bytes());
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        bytes[12..16].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Self {
        bytes
            .get(..Self::ENCODED_BYTES)
            .and_then(Self::decode_block)
            .unwrap_or(Self::UNKNOWN)
    }

    fn decode_block(bytes: &[u8]) -> Option<Self> {
        let mut checksum = [0_u8; 4];
        checksum.copy_from_slice(&bytes[12..16]);
        let valid = bytes[0..4] == Self::MAGIC
            && Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]) == u32::from_le_bytes(checksum);
        let mut value = [0_u8; 8];
        value.copy_from_slice(&bytes[4..12]);
        valid.then_some(Self(u64::from_le_bytes(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::CollectionIncarnation;

    #[test]
    fn round_trips() {
        let incarnation = CollectionIncarnation::fresh().unwrap();
        assert_eq!(
            CollectionIncarnation::decode(&incarnation.encode()),
            incarnation
        );
    }

    #[test]
    fn a_short_file_is_unknown() {
        let encoded = CollectionIncarnation::fresh().unwrap().encode();
        assert_eq!(
            CollectionIncarnation::decode(&encoded[..15]),
            CollectionIncarnation::UNKNOWN
        );
    }

    #[test]
    fn a_damaged_file_is_unknown() {
        let mut encoded = CollectionIncarnation::fresh().unwrap().encode();
        encoded[5] ^= 0x40;
        assert_eq!(
            CollectionIncarnation::decode(&encoded),
            CollectionIncarnation::UNKNOWN
        );
    }

    #[test]
    fn fresh_incarnations_increase_and_are_known() {
        let first = CollectionIncarnation::fresh().unwrap();
        let second = CollectionIncarnation::fresh().unwrap();
        assert!(second.0 > first.0);
        assert!(first.is_known());
        assert!(!CollectionIncarnation::UNKNOWN.is_known());
    }
}
