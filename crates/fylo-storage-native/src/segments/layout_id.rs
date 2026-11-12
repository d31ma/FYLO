use fylo_format::Crc32c;

use crate::NativeStorageError;

/// Names one segment layout from its creation to its removal (ADR 0027).
///
/// A directory collection keeps it once, in a 16-byte block after its two
/// generation slots in `state.slots`. It is written when the layout is
/// created and never again, so a publication leaves it as it is. It is a
/// TTID clock tick: a layout created at a path after another one was
/// removed from it gets a later tick, so the two never share an id, and
/// no process can take a cached location table of one for the other.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct LayoutId(u64);

impl LayoutId {
    /// The id of a layout that has none: one written before layout ids, a
    /// medium that names its layout in its identity, or a damaged block.
    pub(crate) const UNKNOWN: Self = Self(0);
    pub(crate) const ENCODED_BYTES: usize = 16;
    const MAGIC: [u8; 4] = *b"FYLI";
    const CHECKED_BYTES: usize = 12;

    /// The id of a layout created now.
    pub(crate) fn fresh() -> Result<Self, NativeStorageError> {
        crate::write::next_ttid_tick().map(Self)
    }

    pub(crate) fn encode(self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        bytes[0..4].copy_from_slice(&Self::MAGIC);
        bytes[4..12].copy_from_slice(&self.0.to_le_bytes());
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        bytes[12..16].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    /// The id a block holds, or [`Self::UNKNOWN`] when the block is missing,
    /// short or fails its checksum.
    pub(crate) fn decode(bytes: &[u8]) -> Self {
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
    use super::LayoutId;

    #[test]
    fn round_trips() {
        let id = LayoutId(0x0123_4567_89AB_CDEF);
        assert_eq!(LayoutId::decode(&id.encode()), id);
    }

    #[test]
    fn a_missing_block_is_unknown() {
        assert_eq!(LayoutId::decode(&[]), LayoutId::UNKNOWN);
    }

    #[test]
    fn a_short_block_is_unknown() {
        let bytes = LayoutId(7).encode();
        assert_eq!(LayoutId::decode(&bytes[..15]), LayoutId::UNKNOWN);
    }

    #[test]
    fn a_torn_block_is_unknown() {
        let mut bytes = LayoutId(7).encode();
        bytes[6] ^= 1;
        assert_eq!(LayoutId::decode(&bytes), LayoutId::UNKNOWN);
    }

    #[test]
    fn a_zeroed_block_is_unknown() {
        assert_eq!(LayoutId::decode(&[0_u8; 16]), LayoutId::UNKNOWN);
    }

    #[test]
    fn fresh_ids_increase_and_are_never_unknown() {
        let first = LayoutId::fresh().unwrap();
        let second = LayoutId::fresh().unwrap();
        assert!(first > LayoutId::UNKNOWN);
        assert!(second > first);
    }
}
