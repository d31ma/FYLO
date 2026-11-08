use crate::container_bytes::{read_u32, read_u64, write_u32, write_u64};
use crate::{ContainerCollectionHeader, ContainerFormatError, Crc32c};

/// One copy of the directory journal: the static part a writer is about to
/// write in place over a collection's directory block, kept first so that a
/// write a power cut tears can be finished from it. The two
/// [`ContainerDirectoryJournal::SLOT_BYTES`] before the access slots (or at
/// the end of the directory extent's payload, when the file is not
/// protected) hold copies A and B. A writer writes the copy that does not
/// hold the newest valid generation and flushes it before it touches the
/// block, so a stop leaves the block whole or the newest copy naming it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerDirectoryJournal {
    /// Each write takes the next generation.
    pub generation: u64,
    /// The directory block the static part belongs to.
    pub block: u32,
    /// The static part, exactly as the block holds it: plain or sealed.
    pub static_part: Vec<u8>,
}

impl ContainerDirectoryJournal {
    /// Bytes each copy takes in the directory extent.
    pub const SLOT_BYTES: usize = 4096;
    /// Magic at the start of a copy.
    pub const MAGIC: [u8; 8] = *b"VDXDJRN\0";
    const GENERATION_AT: usize = 8;
    const BLOCK_AT: usize = 16;
    const CHECKSUM_AT: usize = 20;
    const HEADER_BYTES: usize = 24;
    const ENCODED_BYTES: usize = Self::HEADER_BYTES + ContainerCollectionHeader::ENCODED_BYTES;

    /// Encode the copy: header, then the static part. The checksum covers
    /// both.
    ///
    /// # Errors
    ///
    /// Returns `InvalidField` for a static part that is not
    /// [`ContainerCollectionHeader::ENCODED_BYTES`] long.
    pub fn encode(&self) -> Result<Vec<u8>, ContainerFormatError> {
        Self::require_static_length(&self.static_part)?;
        let mut bytes = vec![0_u8; Self::HEADER_BYTES];
        bytes[..8].copy_from_slice(&Self::MAGIC);
        write_u64(&mut bytes, Self::GENERATION_AT, self.generation);
        write_u32(&mut bytes, Self::BLOCK_AT, self.block);
        bytes.extend_from_slice(&self.static_part);
        let checksum = Self::checksum(&bytes);
        write_u32(&mut bytes, Self::CHECKSUM_AT, checksum);
        Ok(bytes)
    }

    /// Decode and verify one copy.
    ///
    /// # Errors
    ///
    /// Returns `Truncated`, `BadMagic` or `ChecksumMismatch`.
    pub fn decode(bytes: &[u8]) -> Result<ContainerDirectoryJournal, ContainerFormatError> {
        let whole = bytes
            .get(..Self::ENCODED_BYTES)
            .ok_or(ContainerFormatError::Truncated)?;
        Self::require_magic(whole)?;
        Self::require_checksum(whole)?;
        Ok(ContainerDirectoryJournal {
            generation: read_u64(whole, Self::GENERATION_AT),
            block: read_u32(whole, Self::BLOCK_AT),
            static_part: whole[Self::HEADER_BYTES..].to_vec(),
        })
    }

    /// The valid copy with the higher generation, and whether it is copy A;
    /// `None` when neither copy is valid, as in a file never written.
    #[must_use]
    pub fn newest(copy_a: &[u8], copy_b: &[u8]) -> Option<(ContainerDirectoryJournal, bool)> {
        let a = Self::decode(copy_a).ok();
        let b = Self::decode(copy_b).ok();
        Self::pick(a, b)
    }

    fn pick(
        a: Option<ContainerDirectoryJournal>,
        b: Option<ContainerDirectoryJournal>,
    ) -> Option<(ContainerDirectoryJournal, bool)> {
        match (a, b) {
            (Some(a), Some(b)) if b.generation > a.generation => Some((b, false)),
            (Some(a), _) => Some((a, true)),
            (None, b) => b.map(|b| (b, false)),
        }
    }

    fn require_static_length(static_part: &[u8]) -> Result<(), ContainerFormatError> {
        Some(static_part)
            .filter(|part| part.len() == ContainerCollectionHeader::ENCODED_BYTES)
            .map(|_| ())
            .ok_or(ContainerFormatError::InvalidField)
    }

    fn require_magic(whole: &[u8]) -> Result<(), ContainerFormatError> {
        Some(whole)
            .filter(|whole| whole[..8] == Self::MAGIC)
            .map(|_| ())
            .ok_or(ContainerFormatError::BadMagic)
    }

    fn require_checksum(whole: &[u8]) -> Result<(), ContainerFormatError> {
        Some(whole)
            .filter(|whole| Self::checksum(whole) == read_u32(whole, Self::CHECKSUM_AT))
            .map(|_| ())
            .ok_or(ContainerFormatError::ChecksumMismatch)
    }

    fn checksum(whole: &[u8]) -> u32 {
        let mut hashed = whole.to_vec();
        hashed[Self::CHECKSUM_AT..Self::HEADER_BYTES].fill(0);
        Crc32c::checksum(&hashed)
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerDirectoryJournal;
    use crate::{ContainerCollectionHeader, ContainerFormatError};

    fn copy(generation: u64, block: u32, fill: u8) -> Vec<u8> {
        let mut bytes = ContainerDirectoryJournal {
            generation,
            block,
            static_part: vec![fill; ContainerCollectionHeader::ENCODED_BYTES],
        }
        .encode()
        .unwrap();
        bytes.resize(ContainerDirectoryJournal::SLOT_BYTES, 0);
        bytes
    }

    #[test]
    fn a_copy_fits_its_slot() {
        assert_eq!(copy(1, 0, 7).len(), ContainerDirectoryJournal::SLOT_BYTES);
    }

    #[test]
    fn decodes_what_it_encodes() {
        let decoded = ContainerDirectoryJournal::decode(&copy(3, 9, 7)).unwrap();
        assert_eq!((decoded.generation, decoded.block), (3, 9));
        assert_eq!(
            decoded.static_part,
            vec![7; ContainerCollectionHeader::ENCODED_BYTES]
        );
    }

    #[test]
    fn a_flipped_byte_fails_the_checksum() {
        let mut bytes = copy(3, 9, 7);
        bytes[600] ^= 1;
        assert_eq!(
            ContainerDirectoryJournal::decode(&bytes),
            Err(ContainerFormatError::ChecksumMismatch)
        );
    }

    #[test]
    fn zeros_are_no_copy() {
        assert_eq!(
            ContainerDirectoryJournal::decode(&[0; ContainerDirectoryJournal::SLOT_BYTES]),
            Err(ContainerFormatError::BadMagic)
        );
    }

    #[test]
    fn a_static_part_of_the_wrong_length_is_refused() {
        let encoded = ContainerDirectoryJournal {
            generation: 1,
            block: 0,
            static_part: vec![0; 10],
        }
        .encode();
        assert_eq!(encoded, Err(ContainerFormatError::InvalidField));
    }

    #[test]
    fn a_torn_newer_copy_falls_back_to_the_older() {
        let mut torn = copy(5, 2, 8);
        torn[512..].fill(0);
        let (newest, in_a) = ContainerDirectoryJournal::newest(&copy(4, 1, 7), &torn).unwrap();
        assert_eq!((newest.block, in_a), (1, true));
    }

    #[test]
    fn the_newer_valid_copy_wins() {
        let (newest, in_a) =
            ContainerDirectoryJournal::newest(&copy(4, 1, 7), &copy(5, 2, 8)).unwrap();
        assert_eq!((newest.block, in_a), (2, false));
    }
}
