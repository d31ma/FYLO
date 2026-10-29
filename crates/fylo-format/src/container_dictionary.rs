use crate::container_bytes::{all_zero, read_u32, write_u32};
use crate::{ContainerFormatError, Crc32c};

/// The header at the start of a `dictionary` extent's payload in a `.vdbx`
/// container (ADR 0016 sections 3 and 8). The stored dictionary follows it:
/// a zstd dictionary, sealed in an encrypted file.
///
/// The header names the dictionary's id, so a collection's dictionaries are
/// found from the map's `dictionary` extents alone, and it carries a CRC-32C
/// of the stored bytes, so `fylo verify` checks them without the key. The
/// checkpoint slot names only the current dictionary; older ones stay
/// readable for as long as a record names them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerDictionaryHeader {
    /// The id frames name the dictionary by; never zero.
    pub id: u32,
    /// Whether the stored bytes are sealed with the file key.
    pub sealed: bool,
    /// Bytes stored after the header.
    pub length: u32,
    /// CRC-32C of the stored bytes.
    pub checksum: u32,
}

impl ContainerDictionaryHeader {
    /// Encoded size of the header.
    pub const ENCODED_BYTES: usize = 32;
    /// Most bytes a stored dictionary may hold: far more than the 32 KiB a
    /// collection trains, and less than any extent's payload.
    pub const MAX_STORED_BYTES: u32 = 512 * 1024;
    const MAGIC: [u8; 4] = *b"VDXC";
    const SEALED: u8 = 1;
    const CHECKED_BYTES: usize = Self::ENCODED_BYTES - 4;

    /// The header for `stored`, the bytes that follow it.
    ///
    /// # Errors
    ///
    /// Returns `InvalidField` for a zero id and `Oversized` for more than
    /// [`Self::MAX_STORED_BYTES`].
    pub fn for_stored(id: u32, sealed: bool, stored: &[u8]) -> Result<Self, ContainerFormatError> {
        if id == 0 {
            return Err(ContainerFormatError::InvalidField);
        }
        let length = u32::try_from(stored.len())
            .ok()
            .filter(|length| *length <= Self::MAX_STORED_BYTES)
            .ok_or(ContainerFormatError::Oversized)?;
        Ok(Self {
            id,
            sealed,
            length,
            checksum: Crc32c::checksum(stored),
        })
    }

    /// Encode the header with its own checksum.
    #[must_use]
    pub fn encode(&self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        bytes[0..4].copy_from_slice(&Self::MAGIC);
        if self.sealed {
            bytes[4] = Self::SEALED;
        }
        write_u32(&mut bytes, 8, self.id);
        write_u32(&mut bytes, 12, self.length);
        write_u32(&mut bytes, 16, self.checksum);
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        write_u32(&mut bytes, Self::CHECKED_BYTES, checksum);
        bytes
    }

    /// Decode and verify a header.
    ///
    /// # Errors
    ///
    /// Returns the first defect: too few bytes, wrong magic, a checksum
    /// mismatch, a zero id, unknown flags or non-zero reserved bytes, or a
    /// stored length above [`Self::MAX_STORED_BYTES`].
    pub fn decode(bytes: &[u8]) -> Result<Self, ContainerFormatError> {
        let bytes = bytes
            .get(..Self::ENCODED_BYTES)
            .ok_or(ContainerFormatError::Truncated)?;
        if bytes[0..4] != Self::MAGIC {
            return Err(ContainerFormatError::BadMagic);
        }
        if Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]) != read_u32(bytes, Self::CHECKED_BYTES) {
            return Err(ContainerFormatError::ChecksumMismatch);
        }
        let header = Self {
            id: read_u32(bytes, 8),
            sealed: bytes[4] == Self::SEALED,
            length: read_u32(bytes, 12),
            checksum: read_u32(bytes, 16),
        };
        if header.id == 0
            || bytes[4] & !Self::SEALED != 0
            || !all_zero(&bytes[5..8])
            || !all_zero(&bytes[20..Self::CHECKED_BYTES])
        {
            return Err(ContainerFormatError::InvalidField);
        }
        if header.length > Self::MAX_STORED_BYTES {
            return Err(ContainerFormatError::Oversized);
        }
        Ok(header)
    }

    /// Check the stored bytes against the header.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` or `InvalidField` for a length other than the
    /// header's, and `ChecksumMismatch` when they do not match its CRC-32C.
    pub fn check(&self, stored: &[u8]) -> Result<(), ContainerFormatError> {
        if stored.len() != self.length as usize {
            return Err(ContainerFormatError::InvalidField);
        }
        if Crc32c::checksum(stored) != self.checksum {
            return Err(ContainerFormatError::ChecksumMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerDictionaryHeader;
    use crate::ContainerFormatError;

    #[test]
    fn round_trips_and_checks_its_bytes() {
        let stored = vec![7_u8; 900];
        let header = ContainerDictionaryHeader::for_stored(3, true, &stored).unwrap();
        assert_eq!(
            ContainerDictionaryHeader::decode(&header.encode()),
            Ok(header)
        );
        assert_eq!(header.check(&stored), Ok(()));
        assert_eq!(
            header.check(&stored[1..]),
            Err(ContainerFormatError::InvalidField)
        );
        let mut damaged = stored;
        damaged[400] ^= 1;
        assert_eq!(
            header.check(&damaged),
            Err(ContainerFormatError::ChecksumMismatch)
        );
    }

    #[test]
    fn refuses_damage_and_undefined_fields() {
        assert_eq!(
            ContainerDictionaryHeader::for_stored(0, false, b"x"),
            Err(ContainerFormatError::InvalidField)
        );
        let header = ContainerDictionaryHeader::for_stored(1, false, b"dictionary").unwrap();
        let bytes = header.encode();
        assert_eq!(
            ContainerDictionaryHeader::decode(&bytes[..31]),
            Err(ContainerFormatError::Truncated)
        );
        assert_eq!(
            ContainerDictionaryHeader::decode(&[0; 32]),
            Err(ContainerFormatError::BadMagic)
        );
        let mut flipped = bytes;
        flipped[9] ^= 1;
        assert_eq!(
            ContainerDictionaryHeader::decode(&flipped),
            Err(ContainerFormatError::ChecksumMismatch)
        );
        for (at, value) in [(4, 2), (6, 1), (24, 1)] {
            let mut bytes = bytes;
            bytes[at] = value;
            let checksum = crate::Crc32c::checksum(&bytes[..28]);
            bytes[28..].copy_from_slice(&checksum.to_le_bytes());
            assert_eq!(
                ContainerDictionaryHeader::decode(&bytes),
                Err(ContainerFormatError::InvalidField),
                "byte {at}"
            );
        }
    }
}
