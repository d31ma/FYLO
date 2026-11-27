use crate::container_bytes::{read_u32, read_u64, write_u32, write_u64};
use crate::{ContainerFormatError, Crc32c};

/// One copy of a protected file's access document (ADR 0021): the signed
/// policy versions and the key wraps. The last two
/// [`ContainerAccessSlot::SLOT_BYTES`] of the directory extent's payload
/// hold copies A and B. A change writes the copy that does not hold the
/// newest valid generation and flushes it, so a stop leaves the old
/// document or the new one whole, as the superblock copies do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerAccessSlot {
    /// Each change writes the next generation.
    pub generation: u64,
    /// The access document.
    pub body: Vec<u8>,
}

impl ContainerAccessSlot {
    /// Bytes each copy takes in the directory extent.
    pub const SLOT_BYTES: usize = 256 * 1024;
    /// Magic at the start of a copy.
    pub const MAGIC: [u8; 8] = *b"VDXACCS\0";
    const GENERATION_AT: usize = 8;
    const LENGTH_AT: usize = 16;
    const CHECKSUM_AT: usize = 20;
    const HEADER_BYTES: usize = 24;
    /// The largest access document a copy holds.
    pub const MAX_BODY_BYTES: usize = Self::SLOT_BYTES - Self::HEADER_BYTES;

    /// Encode the copy: header, then the document. The checksum covers both.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` for a document past [`Self::MAX_BODY_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, ContainerFormatError> {
        let length = Self::body_length(&self.body)?;
        let mut bytes = vec![0_u8; Self::HEADER_BYTES];
        bytes[..8].copy_from_slice(&Self::MAGIC);
        write_u64(&mut bytes, Self::GENERATION_AT, self.generation);
        write_u32(&mut bytes, Self::LENGTH_AT, length);
        bytes.extend_from_slice(&self.body);
        let checksum = Self::checksum(&bytes);
        write_u32(&mut bytes, Self::CHECKSUM_AT, checksum);
        Ok(bytes)
    }

    /// Decode and verify one copy.
    ///
    /// # Errors
    ///
    /// Returns `Truncated`, `BadMagic`, `Oversized` or `ChecksumMismatch`.
    pub fn decode(bytes: &[u8]) -> Result<ContainerAccessSlot, ContainerFormatError> {
        let header = bytes
            .get(..Self::HEADER_BYTES)
            .ok_or(ContainerFormatError::Truncated)?;
        Self::require_magic(header)?;
        let length = Self::declared_length(header)?;
        let whole = bytes
            .get(..Self::HEADER_BYTES + length)
            .ok_or(ContainerFormatError::Truncated)?;
        Self::require_checksum(whole)?;
        Ok(ContainerAccessSlot {
            generation: read_u64(header, Self::GENERATION_AT),
            body: whole[Self::HEADER_BYTES..].to_vec(),
        })
    }

    /// The valid copy with the higher generation, and whether it is copy A;
    /// `None` when neither copy is valid.
    #[must_use]
    pub fn newest(copy_a: &[u8], copy_b: &[u8]) -> Option<(ContainerAccessSlot, bool)> {
        let a = Self::decode(copy_a).ok();
        let b = Self::decode(copy_b).ok();
        Self::pick(a, b)
    }

    fn pick(
        a: Option<ContainerAccessSlot>,
        b: Option<ContainerAccessSlot>,
    ) -> Option<(ContainerAccessSlot, bool)> {
        match (a, b) {
            (Some(a), Some(b)) if b.generation > a.generation => Some((b, false)),
            (Some(a), _) => Some((a, true)),
            (None, b) => b.map(|b| (b, false)),
        }
    }

    fn body_length(body: &[u8]) -> Result<u32, ContainerFormatError> {
        Some(body.len())
            .filter(|length| *length <= Self::MAX_BODY_BYTES)
            .and_then(|length| u32::try_from(length).ok())
            .ok_or(ContainerFormatError::Oversized)
    }

    fn declared_length(header: &[u8]) -> Result<usize, ContainerFormatError> {
        usize::try_from(read_u32(header, Self::LENGTH_AT))
            .ok()
            .filter(|length| *length <= Self::MAX_BODY_BYTES)
            .ok_or(ContainerFormatError::Oversized)
    }

    fn require_magic(header: &[u8]) -> Result<(), ContainerFormatError> {
        Some(header)
            .filter(|header| header[..8] == Self::MAGIC)
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
    use super::ContainerAccessSlot;
    use crate::ContainerFormatError;

    fn slot(generation: u64, body: &[u8]) -> Vec<u8> {
        let mut bytes = ContainerAccessSlot {
            generation,
            body: body.to_vec(),
        }
        .encode()
        .unwrap();
        bytes.resize(ContainerAccessSlot::SLOT_BYTES, 0);
        bytes
    }

    #[test]
    fn decodes_what_it_encodes() {
        let decoded = ContainerAccessSlot::decode(&slot(3, b"{\"format\":1}")).unwrap();
        assert_eq!(decoded.generation, 3);
        assert_eq!(decoded.body, b"{\"format\":1}");
    }

    #[test]
    fn a_flipped_byte_fails_the_checksum() {
        let mut bytes = slot(3, b"{\"format\":1}");
        bytes[30] ^= 1;
        assert_eq!(
            ContainerAccessSlot::decode(&bytes),
            Err(ContainerFormatError::ChecksumMismatch)
        );
    }

    #[test]
    fn zeros_are_no_slot() {
        assert_eq!(
            ContainerAccessSlot::decode(&[0; 64]),
            Err(ContainerFormatError::BadMagic)
        );
    }

    #[test]
    fn refuses_a_document_too_large_for_a_slot() {
        let body = vec![b' '; ContainerAccessSlot::MAX_BODY_BYTES + 1];
        let encoded = ContainerAccessSlot {
            generation: 1,
            body,
        }
        .encode();
        assert_eq!(encoded, Err(ContainerFormatError::Oversized));
    }

    #[test]
    fn the_newer_valid_copy_wins() {
        let (newest, in_a) = ContainerAccessSlot::newest(&slot(4, b"a"), &slot(5, b"b")).unwrap();
        assert_eq!((newest.body.as_slice(), in_a), (b"b".as_slice(), false));
    }

    #[test]
    fn a_damaged_newer_copy_falls_back_to_the_older() {
        let mut damaged = slot(5, b"b");
        damaged[24] ^= 1;
        let (newest, in_a) = ContainerAccessSlot::newest(&slot(4, b"a"), &damaged).unwrap();
        assert_eq!((newest.body.as_slice(), in_a), (b"a".as_slice(), true));
    }

    #[test]
    fn neither_copy_valid_is_none() {
        assert_eq!(ContainerAccessSlot::newest(&[0; 64], &[0; 64]), None);
    }
}
