use crate::container_bytes::{all_zero, read_u16, read_u32, write_u16, write_u32};
use crate::{ContainerFormatError, Crc32c};

/// The static part of a collection's 4 KiB block in the collection directory
/// of a `.fxdb` container (ADR 0016 section 3).
///
/// It is written when the collection is created and only replaced, copy on
/// write, by a directory change such as a rename or a drop. The checkpoint and
/// generation slots that change at run time follow it in the same block, each
/// with its own checksum, at [`Self::CHECKPOINT_SLOTS_OFFSET`] and
/// [`Self::GENERATION_SLOTS_OFFSET`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerCollectionHeader {
    /// Collection id, unique within the file and never reused; at least 1.
    pub id: u32,
    /// Collection name, at most [`Self::MAX_NAME_BYTES`] bytes of UTF-8.
    pub name: String,
    /// Whether the collection has been dropped (its extents await release).
    pub dropped: bool,
    /// The collection's catalog descriptor (canonical JSON), at most
    /// [`Self::MAX_DESCRIPTOR_BYTES`] bytes.
    pub descriptor: Vec<u8>,
}

impl ContainerCollectionHeader {
    /// Size of a whole collection block.
    pub const BLOCK_BYTES: usize = 4096;
    /// Encoded size of the static part.
    pub const ENCODED_BYTES: usize = 3456;
    /// Offset of the two 256-byte checkpoint slots within the block.
    pub const CHECKPOINT_SLOTS_OFFSET: usize = 3456;
    /// Offset of the two 64-byte generation slots within the block.
    pub const GENERATION_SLOTS_OFFSET: usize = 3968;
    /// Longest collection name.
    pub const MAX_NAME_BYTES: usize = 255;
    /// Longest catalog descriptor.
    pub const MAX_DESCRIPTOR_BYTES: usize = 3072;
    /// Current version.
    pub const VERSION: u16 = 1;
    /// Longest sealed body [`Self::encode_sealed`] accepts.
    pub const MAX_SEALED_BYTES: usize = Self::ENCODED_BYTES - Self::SEALED_OFFSET - 4;
    const MAGIC: [u8; 8] = *b"FXCOLL\0\0";
    const SEALED_MAGIC: [u8; 8] = *b"FXCOLLS\0";
    const SEALED_OFFSET: usize = 16;
    const BODY_FIXED_BYTES: usize = 11;
    const NAME_OFFSET: usize = 24;
    const DESCRIPTOR_OFFSET: usize = 280;
    const CHECKED_BYTES: usize = Self::ENCODED_BYTES - 4;

    /// Encode the static part with its checksum.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` for a name or descriptor over its limit, and
    /// `InvalidField` for an id of zero or an empty name.
    pub fn encode(&self) -> Result<Vec<u8>, ContainerFormatError> {
        if self.name.len() > Self::MAX_NAME_BYTES
            || self.descriptor.len() > Self::MAX_DESCRIPTOR_BYTES
        {
            return Err(ContainerFormatError::Oversized);
        }
        if self.id == 0 || self.name.is_empty() {
            return Err(ContainerFormatError::InvalidField);
        }
        let name_length =
            u16::try_from(self.name.len()).map_err(|_| ContainerFormatError::Oversized)?;
        let descriptor_length =
            u32::try_from(self.descriptor.len()).map_err(|_| ContainerFormatError::Oversized)?;
        let mut bytes = vec![0_u8; Self::ENCODED_BYTES];
        bytes[0..8].copy_from_slice(&Self::MAGIC);
        write_u16(&mut bytes, 8, Self::VERSION);
        write_u16(&mut bytes, 10, name_length);
        write_u32(&mut bytes, 12, self.id);
        bytes[16] = if self.dropped { 2 } else { 1 };
        write_u32(&mut bytes, 20, descriptor_length);
        bytes[Self::NAME_OFFSET..Self::NAME_OFFSET + self.name.len()]
            .copy_from_slice(self.name.as_bytes());
        bytes[Self::DESCRIPTOR_OFFSET..Self::DESCRIPTOR_OFFSET + self.descriptor.len()]
            .copy_from_slice(&self.descriptor);
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        write_u32(&mut bytes, Self::CHECKED_BYTES, checksum);
        Ok(bytes)
    }

    /// Decode and verify the static part from the start of a block.
    ///
    /// # Errors
    ///
    /// Returns the first defect: too few bytes, wrong magic, a checksum
    /// mismatch, an unknown version, an over-long name or descriptor, a name
    /// that is not UTF-8, an unknown state, or non-zero reserved bytes.
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
        let name_length = usize::from(read_u16(bytes, 10));
        let descriptor_length =
            usize::try_from(read_u32(bytes, 20)).map_err(|_| ContainerFormatError::Oversized)?;
        if name_length > Self::MAX_NAME_BYTES || descriptor_length > Self::MAX_DESCRIPTOR_BYTES {
            return Err(ContainerFormatError::Oversized);
        }
        let dropped = match bytes[16] {
            1 => false,
            2 => true,
            _ => return Err(ContainerFormatError::InvalidField),
        };
        let name_end = Self::NAME_OFFSET + name_length;
        let descriptor_end = Self::DESCRIPTOR_OFFSET + descriptor_length;
        let reserved_clear = all_zero(&bytes[17..20])
            && all_zero(&bytes[name_end..Self::DESCRIPTOR_OFFSET])
            && all_zero(&bytes[descriptor_end..Self::CHECKED_BYTES]);
        let id = read_u32(bytes, 12);
        if !reserved_clear || id == 0 || name_length == 0 {
            return Err(ContainerFormatError::InvalidField);
        }
        let name = std::str::from_utf8(&bytes[Self::NAME_OFFSET..name_end])
            .map_err(|_| ContainerFormatError::InvalidField)?
            .to_owned();
        Ok(Self {
            id,
            name,
            dropped,
            descriptor: bytes[Self::DESCRIPTOR_OFFSET..descriptor_end].to_vec(),
        })
    }
}

impl ContainerCollectionHeader {
    /// Encode the fields alone, compactly, for an encrypted file to seal:
    /// id, state, name length, descriptor length, name, descriptor.
    ///
    /// # Errors
    ///
    /// The same as [`Self::encode`].
    pub fn encode_body(&self) -> Result<Vec<u8>, ContainerFormatError> {
        if self.name.len() > Self::MAX_NAME_BYTES
            || self.descriptor.len() > Self::MAX_DESCRIPTOR_BYTES
        {
            return Err(ContainerFormatError::Oversized);
        }
        if self.id == 0 || self.name.is_empty() {
            return Err(ContainerFormatError::InvalidField);
        }
        let name_length =
            u16::try_from(self.name.len()).map_err(|_| ContainerFormatError::Oversized)?;
        let descriptor_length =
            u32::try_from(self.descriptor.len()).map_err(|_| ContainerFormatError::Oversized)?;
        let mut bytes = vec![0_u8; Self::BODY_FIXED_BYTES];
        write_u32(&mut bytes, 0, self.id);
        bytes[4] = if self.dropped { 2 } else { 1 };
        write_u16(&mut bytes, 5, name_length);
        write_u32(&mut bytes, 7, descriptor_length);
        bytes.extend_from_slice(self.name.as_bytes());
        bytes.extend_from_slice(&self.descriptor);
        Ok(bytes)
    }

    /// Decode fields encoded by [`Self::encode_body`].
    ///
    /// # Errors
    ///
    /// Returns `Truncated` or `InvalidField` for bytes of the wrong length,
    /// and the field checks of [`Self::decode`].
    pub fn decode_body(bytes: &[u8]) -> Result<Self, ContainerFormatError> {
        if bytes.len() < Self::BODY_FIXED_BYTES {
            return Err(ContainerFormatError::Truncated);
        }
        let name_length = usize::from(read_u16(bytes, 5));
        let descriptor_length =
            usize::try_from(read_u32(bytes, 7)).map_err(|_| ContainerFormatError::Oversized)?;
        if name_length > Self::MAX_NAME_BYTES || descriptor_length > Self::MAX_DESCRIPTOR_BYTES {
            return Err(ContainerFormatError::Oversized);
        }
        let name_end = Self::BODY_FIXED_BYTES + name_length;
        if bytes.len() != name_end + descriptor_length {
            return Err(ContainerFormatError::InvalidField);
        }
        let dropped = match bytes[4] {
            1 => false,
            2 => true,
            _ => return Err(ContainerFormatError::InvalidField),
        };
        let id = read_u32(bytes, 0);
        if id == 0 || name_length == 0 {
            return Err(ContainerFormatError::InvalidField);
        }
        let name = std::str::from_utf8(&bytes[Self::BODY_FIXED_BYTES..name_end])
            .map_err(|_| ContainerFormatError::InvalidField)?
            .to_owned();
        Ok(Self {
            id,
            name,
            dropped,
            descriptor: bytes[name_end..].to_vec(),
        })
    }

    /// Encode the static part of an encrypted file around a sealed body.
    /// The checksum covers the sealed bytes, so a verifier without the key
    /// can still find damage.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` for a body over [`Self::MAX_SEALED_BYTES`].
    pub fn encode_sealed(sealed: &[u8]) -> Result<Vec<u8>, ContainerFormatError> {
        if sealed.len() > Self::MAX_SEALED_BYTES {
            return Err(ContainerFormatError::Oversized);
        }
        let length = u32::try_from(sealed.len()).map_err(|_| ContainerFormatError::Oversized)?;
        let mut bytes = vec![0_u8; Self::ENCODED_BYTES];
        bytes[0..8].copy_from_slice(&Self::SEALED_MAGIC);
        write_u16(&mut bytes, 8, Self::VERSION);
        write_u32(&mut bytes, 12, length);
        bytes[Self::SEALED_OFFSET..Self::SEALED_OFFSET + sealed.len()].copy_from_slice(sealed);
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        write_u32(&mut bytes, Self::CHECKED_BYTES, checksum);
        Ok(bytes)
    }

    /// Decode and verify the static part of an encrypted file, returning the
    /// sealed body.
    ///
    /// # Errors
    ///
    /// Returns the first defect: too few bytes, wrong magic, a checksum
    /// mismatch, an unknown version, an over-long body, or non-zero reserved
    /// bytes.
    pub fn decode_sealed(bytes: &[u8]) -> Result<&[u8], ContainerFormatError> {
        let bytes = bytes
            .get(..Self::ENCODED_BYTES)
            .ok_or(ContainerFormatError::Truncated)?;
        if bytes[0..8] != Self::SEALED_MAGIC {
            return Err(ContainerFormatError::BadMagic);
        }
        if Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]) != read_u32(bytes, Self::CHECKED_BYTES) {
            return Err(ContainerFormatError::ChecksumMismatch);
        }
        if read_u16(bytes, 8) != Self::VERSION {
            return Err(ContainerFormatError::UnsupportedVersion);
        }
        let length =
            usize::try_from(read_u32(bytes, 12)).map_err(|_| ContainerFormatError::Oversized)?;
        if length > Self::MAX_SEALED_BYTES {
            return Err(ContainerFormatError::Oversized);
        }
        let end = Self::SEALED_OFFSET + length;
        if !all_zero(&bytes[10..12]) || !all_zero(&bytes[end..Self::CHECKED_BYTES]) {
            return Err(ContainerFormatError::InvalidField);
        }
        Ok(&bytes[Self::SEALED_OFFSET..end])
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerCollectionHeader;
    use crate::ContainerFormatError;

    fn sample() -> ContainerCollectionHeader {
        ContainerCollectionHeader {
            id: 4,
            name: "orders".to_owned(),
            dropped: false,
            descriptor: br#"{"kind":"document","layout":"fxdb"}"#.to_vec(),
        }
    }

    #[test]
    fn round_trips() {
        let header = sample();
        assert_eq!(
            ContainerCollectionHeader::decode(&header.encode().unwrap()),
            Ok(header)
        );
    }

    #[test]
    fn slots_follow_the_static_part() {
        assert_eq!(
            ContainerCollectionHeader::ENCODED_BYTES,
            ContainerCollectionHeader::CHECKPOINT_SLOTS_OFFSET
        );
        assert_eq!(
            ContainerCollectionHeader::CHECKPOINT_SLOTS_OFFSET + 2 * 256,
            ContainerCollectionHeader::GENERATION_SLOTS_OFFSET
        );
        assert_eq!(
            ContainerCollectionHeader::GENERATION_SLOTS_OFFSET + 2 * 64,
            ContainerCollectionHeader::BLOCK_BYTES
        );
    }

    #[test]
    fn enforces_limits() {
        let mut long_name = sample();
        long_name.name = "n".repeat(256);
        assert_eq!(long_name.encode(), Err(ContainerFormatError::Oversized));
        let mut long_descriptor = sample();
        long_descriptor.descriptor = vec![b' '; 3073];
        assert_eq!(
            long_descriptor.encode(),
            Err(ContainerFormatError::Oversized)
        );
        let mut no_id = sample();
        no_id.id = 0;
        assert_eq!(no_id.encode(), Err(ContainerFormatError::InvalidField));
    }

    #[test]
    fn round_trips_a_body_for_sealing() {
        let header = sample();
        let body = header.encode_body().unwrap();
        assert_eq!(ContainerCollectionHeader::decode_body(&body), Ok(header));
        assert_eq!(
            ContainerCollectionHeader::decode_body(&body[..body.len() - 1]),
            Err(ContainerFormatError::InvalidField)
        );
        let mut largest = sample();
        largest.name = "n".repeat(ContainerCollectionHeader::MAX_NAME_BYTES);
        largest.descriptor = vec![b' '; ContainerCollectionHeader::MAX_DESCRIPTOR_BYTES];
        // A nonce and a tag around the largest body still fit.
        assert!(
            largest.encode_body().unwrap().len() + 28
                <= ContainerCollectionHeader::MAX_SEALED_BYTES
        );
    }

    #[test]
    fn wraps_a_sealed_body() {
        let sealed = vec![7_u8; 300];
        let bytes = ContainerCollectionHeader::encode_sealed(&sealed).unwrap();
        assert_eq!(bytes.len(), ContainerCollectionHeader::ENCODED_BYTES);
        assert_eq!(
            ContainerCollectionHeader::decode_sealed(&bytes),
            Ok(&sealed[..])
        );
        assert_eq!(
            ContainerCollectionHeader::decode(&bytes),
            Err(ContainerFormatError::BadMagic)
        );
        let mut flipped = bytes;
        flipped[100] ^= 1;
        assert_eq!(
            ContainerCollectionHeader::decode_sealed(&flipped),
            Err(ContainerFormatError::ChecksumMismatch)
        );
        assert_eq!(
            ContainerCollectionHeader::encode_sealed(
                &[0; ContainerCollectionHeader::MAX_SEALED_BYTES + 1]
            ),
            Err(ContainerFormatError::Oversized)
        );
    }

    #[test]
    fn rejects_damage() {
        let mut flipped = sample().encode().unwrap();
        flipped[30] ^= 1;
        assert_eq!(
            ContainerCollectionHeader::decode(&flipped),
            Err(ContainerFormatError::ChecksumMismatch)
        );
    }
}
