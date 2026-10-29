use crate::container_bytes::{
    all_zero, read_u16, read_u32, read_u64, write_u16, write_u32, write_u64,
};
use crate::{ContainerExtentKind, ContainerFormatError, Crc32c};

/// The header that opens every allocated extent of a `.fxdb` container
/// (ADR 0016 section 3).
///
/// The sequence number is never reused, so a reader holding a location into
/// an extent that was freed and taken again sees a different number and
/// retries instead of reading someone else's bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerExtentHeader {
    /// What the extent holds.
    pub kind: ContainerExtentKind,
    /// Owning collection id; zero for database-level extents.
    pub owner: u32,
    /// Allocation sequence number.
    pub sequence: u64,
    /// The extent's own index, so a header copied to the wrong place is caught.
    pub index: u64,
}

impl ContainerExtentHeader {
    /// Encoded size in bytes.
    pub const ENCODED_BYTES: usize = 64;
    /// Offset of the extent's payload; the rest of the first page is zero so
    /// the payload stays page-aligned.
    pub const PAYLOAD_OFFSET: u64 = 4096;
    /// Current version.
    pub const VERSION: u16 = 1;
    const MAGIC: [u8; 8] = *b"FXEXTNT\0";
    const CHECKED_BYTES: usize = Self::ENCODED_BYTES - 4;

    /// Encode the header with its checksum.
    #[must_use]
    pub fn encode(&self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        bytes[0..8].copy_from_slice(&Self::MAGIC);
        write_u16(&mut bytes, 8, Self::VERSION);
        bytes[10] = self.kind.code();
        write_u32(&mut bytes, 12, self.owner);
        write_u64(&mut bytes, 16, self.sequence);
        write_u64(&mut bytes, 24, self.index);
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        write_u32(&mut bytes, Self::CHECKED_BYTES, checksum);
        bytes
    }

    /// Decode and verify an extent header.
    ///
    /// # Errors
    ///
    /// Returns the first defect: too few bytes, wrong magic, a checksum
    /// mismatch, an unknown version, an unknown kind or non-zero reserved
    /// bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, ContainerFormatError> {
        let bytes = bytes
            .get(..Self::ENCODED_BYTES)
            .ok_or(ContainerFormatError::Truncated)?;
        if bytes[0..8] != Self::MAGIC {
            return Err(ContainerFormatError::BadMagic);
        }
        if Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]) != read_u32(bytes, Self::CHECKED_BYTES) {
            return Err(ContainerFormatError::ChecksumMismatch);
        }
        if read_u16(bytes, 8) != Self::VERSION {
            return Err(ContainerFormatError::UnsupportedVersion);
        }
        if bytes[11] != 0 || !all_zero(&bytes[32..Self::CHECKED_BYTES]) {
            return Err(ContainerFormatError::InvalidField);
        }
        Ok(Self {
            kind: ContainerExtentKind::from_code(bytes[10])?,
            owner: read_u32(bytes, 12),
            sequence: read_u64(bytes, 16),
            index: read_u64(bytes, 24),
        })
    }

    /// Decode the header and check it is the one a location expects.
    ///
    /// # Errors
    ///
    /// Returns any decoding error, and `InvalidField` when the extent now
    /// holds something else: another index, kind, owner or sequence number.
    pub fn expect(
        bytes: &[u8],
        index: u64,
        kind: ContainerExtentKind,
        owner: u32,
        sequence: u64,
    ) -> Result<Self, ContainerFormatError> {
        let header = Self::decode(bytes)?;
        if header.index == index
            && header.kind == kind
            && header.owner == owner
            && header.sequence == sequence
        {
            Ok(header)
        } else {
            Err(ContainerFormatError::InvalidField)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerExtentHeader;
    use crate::{ContainerExtentKind, ContainerFormatError};

    const HEADER: ContainerExtentHeader = ContainerExtentHeader {
        kind: ContainerExtentKind::Segment,
        owner: 3,
        sequence: 41,
        index: 7,
    };

    #[test]
    fn round_trips() {
        assert_eq!(ContainerExtentHeader::decode(&HEADER.encode()), Ok(HEADER));
    }

    #[test]
    fn detects_a_reused_extent() {
        let reused = ContainerExtentHeader {
            sequence: 42,
            ..HEADER
        }
        .encode();
        assert_eq!(
            ContainerExtentHeader::expect(&reused, 7, ContainerExtentKind::Segment, 3, 41),
            Err(ContainerFormatError::InvalidField)
        );
        assert!(
            ContainerExtentHeader::expect(&HEADER.encode(), 7, ContainerExtentKind::Segment, 3, 41)
                .is_ok()
        );
    }

    #[test]
    fn rejects_damage() {
        let mut flipped = HEADER.encode();
        flipped[20] ^= 0x10;
        assert_eq!(
            ContainerExtentHeader::decode(&flipped),
            Err(ContainerFormatError::ChecksumMismatch)
        );
        assert_eq!(
            ContainerExtentHeader::decode(&HEADER.encode()[..63]),
            Err(ContainerFormatError::Truncated)
        );
        assert_eq!(
            ContainerExtentHeader::decode(&[0_u8; 64]),
            Err(ContainerFormatError::BadMagic)
        );
    }
}
