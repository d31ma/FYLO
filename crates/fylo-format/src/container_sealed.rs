use crate::container_bytes::{
    all_zero, read_u16, read_u32, read_u64, write_u16, write_u32, write_u64,
};
use crate::{ContainerFormatError, Crc32c};

/// The block layout of a sealed `.vdbx` file (ADR 0016 sections 8 and 11),
/// at [`Self::OFFSET`], just past the two superblock copies.
///
/// A sealed file is a read-only image of a container: the superblock
/// copies in the clear, then the image's bytes cut into blocks of
/// [`Self::BLOCK_BYTES`], each compressed on its own and, in an encrypted
/// file, sealed with its block number as associated data. The block index
/// after the last block gives each block's place, so a read of any byte
/// range of the image touches only the blocks that hold it. The image's
/// own superblock region is never read from a block: its first
/// [`Self::OFFSET`] bytes are the superblocks this file starts with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerSealedHeader {
    /// Whether every block is sealed with the file key.
    pub encrypted: bool,
    /// Bytes of the image the blocks hold: the container's extent count
    /// times its extent size.
    pub image_bytes: u64,
    /// Blocks in the image, the last one possibly short.
    pub block_count: u64,
    /// Where the block index starts in the file.
    pub index_offset: u64,
    /// CRC-32C of the block index.
    pub index_checksum: u32,
}

impl ContainerSealedHeader {
    /// Where the header sits in the file.
    pub const OFFSET: u64 = 8 * 1024;
    /// Where the first block may start: the header's page is the header's.
    pub const DATA_OFFSET: u64 = 12 * 1024;
    /// Encoded size of the header.
    pub const ENCODED_BYTES: usize = 64;
    /// Bytes of the image one block holds: 64 KiB.
    pub const BLOCK_BYTES: u64 = 1 << Self::BLOCK_SHIFT;
    /// Current version.
    pub const VERSION: u16 = 1;
    const BLOCK_SHIFT: u8 = 16;
    const MAGIC: [u8; 8] = *b"VDXSEALD";
    const ENCRYPTED: u8 = 1;
    const CHECKED_BYTES: usize = Self::ENCODED_BYTES - 4;

    /// Blocks an image of `image_bytes` bytes takes.
    #[must_use]
    pub const fn blocks_for(image_bytes: u64) -> u64 {
        image_bytes.div_ceil(Self::BLOCK_BYTES)
    }

    /// Bytes of the block index.
    #[must_use]
    pub const fn index_bytes(&self) -> u64 {
        self.block_count * ContainerSealedBlock::ENCODED_BYTES as u64
    }

    /// Encode the header with its checksum.
    #[must_use]
    pub fn encode(&self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        bytes[0..8].copy_from_slice(&Self::MAGIC);
        write_u16(&mut bytes, 8, Self::VERSION);
        bytes[10] = Self::BLOCK_SHIFT;
        if self.encrypted {
            bytes[11] = Self::ENCRYPTED;
        }
        write_u64(&mut bytes, 16, self.image_bytes);
        write_u64(&mut bytes, 24, self.block_count);
        write_u64(&mut bytes, 32, self.index_offset);
        write_u32(&mut bytes, 40, self.index_checksum);
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        write_u32(&mut bytes, Self::CHECKED_BYTES, checksum);
        bytes
    }

    /// Decode and verify the header.
    ///
    /// # Errors
    ///
    /// Returns the first defect: too few bytes, wrong magic, a checksum
    /// mismatch, an unknown version, a block size other than 64 KiB,
    /// unknown flags, non-zero reserved bytes, a block count that does not
    /// cover the image, or an index that starts inside the header's page or
    /// cannot fit in a file.
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
        let header = Self {
            encrypted: bytes[11] == Self::ENCRYPTED,
            image_bytes: read_u64(bytes, 16),
            block_count: read_u64(bytes, 24),
            index_offset: read_u64(bytes, 32),
            index_checksum: read_u32(bytes, 40),
        };
        let index_fits = header
            .block_count
            .checked_mul(ContainerSealedBlock::ENCODED_BYTES as u64)
            .and_then(|length| header.index_offset.checked_add(length))
            .is_some();
        if bytes[10] != Self::BLOCK_SHIFT
            || bytes[11] & !Self::ENCRYPTED != 0
            || !all_zero(&bytes[12..16])
            || !all_zero(&bytes[44..Self::CHECKED_BYTES])
            || header.block_count != Self::blocks_for(header.image_bytes)
            || header.index_offset < Self::DATA_OFFSET
            || !index_fits
        {
            return Err(ContainerFormatError::InvalidField);
        }
        Ok(header)
    }

    /// Decode the block index this header describes and check every entry
    /// against it: a block lies between the header's page and the index,
    /// and a hole stores nothing.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` for fewer bytes than the index takes,
    /// `ChecksumMismatch` when they do not match the header's CRC-32C, and
    /// `InvalidField` for an entry outside the blocks' bytes or longer than
    /// a stored block can be.
    pub fn decode_index(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<ContainerSealedBlock>, ContainerFormatError> {
        let length =
            usize::try_from(self.index_bytes()).map_err(|_| ContainerFormatError::Oversized)?;
        let bytes = bytes.get(..length).ok_or(ContainerFormatError::Truncated)?;
        if Crc32c::checksum(bytes) != self.index_checksum {
            return Err(ContainerFormatError::ChecksumMismatch);
        }
        bytes
            .chunks_exact(ContainerSealedBlock::ENCODED_BYTES)
            .map(|entry| {
                let block = ContainerSealedBlock::decode(entry);
                let end = block.offset.checked_add(u64::from(block.length));
                let placed = if block.is_hole() {
                    block.offset == 0 && block.checksum == 0
                } else {
                    block.offset >= Self::DATA_OFFSET
                        && end.is_some_and(|end| end <= self.index_offset)
                        && block.length <= ContainerSealedBlock::MAX_STORED_BYTES
                };
                if placed {
                    Ok(block)
                } else {
                    Err(ContainerFormatError::InvalidField)
                }
            })
            .collect()
    }

    /// Encode a block index, returning its bytes and their CRC-32C for the
    /// header.
    #[must_use]
    pub fn encode_index(blocks: &[ContainerSealedBlock]) -> (Vec<u8>, u32) {
        let mut bytes = Vec::with_capacity(blocks.len() * ContainerSealedBlock::ENCODED_BYTES);
        for block in blocks {
            bytes.extend_from_slice(&block.encode());
        }
        let checksum = Crc32c::checksum(&bytes);
        (bytes, checksum)
    }
}

/// One entry of a sealed file's block index: where a block's stored bytes
/// are and their CRC-32C, which `fylo verify` checks without the key. A
/// block of zeros is a hole and stores nothing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContainerSealedBlock {
    /// Where the stored bytes start; zero for a hole.
    pub offset: u64,
    /// Stored bytes; zero for a hole.
    pub length: u32,
    /// CRC-32C of the stored bytes; zero for a hole.
    pub checksum: u32,
}

impl ContainerSealedBlock {
    /// Encoded size of one entry.
    pub const ENCODED_BYTES: usize = 16;
    /// Most bytes a block may store: the block as is, after a codec byte,
    /// with a nonce and a tag around it, and room to spare.
    pub const MAX_STORED_BYTES: u32 = 64 * 1024 + 64;

    /// Whether the block is a hole: all zeros, stored as nothing.
    #[must_use]
    pub const fn is_hole(&self) -> bool {
        self.length == 0
    }

    /// Encode the entry.
    #[must_use]
    pub fn encode(&self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        write_u64(&mut bytes, 0, self.offset);
        write_u32(&mut bytes, 8, self.length);
        write_u32(&mut bytes, 12, self.checksum);
        bytes
    }

    /// Decode an entry of [`Self::ENCODED_BYTES`]; the index's checksum
    /// covers it, and [`ContainerSealedHeader::decode_index`] checks it.
    fn decode(bytes: &[u8]) -> Self {
        Self {
            offset: read_u64(bytes, 0),
            length: read_u32(bytes, 8),
            checksum: read_u32(bytes, 12),
        }
    }

    /// Check a block's stored bytes against the entry.
    ///
    /// # Errors
    ///
    /// Returns `InvalidField` for a length other than the entry's and
    /// `ChecksumMismatch` when they do not match its CRC-32C.
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
    use super::{ContainerSealedBlock, ContainerSealedHeader};
    use crate::{ContainerFormatError, Crc32c};

    fn blocks() -> Vec<ContainerSealedBlock> {
        let stored = b"a stored block";
        vec![
            ContainerSealedBlock::default(),
            ContainerSealedBlock {
                offset: ContainerSealedHeader::DATA_OFFSET,
                length: u32::try_from(stored.len()).unwrap(),
                checksum: Crc32c::checksum(stored),
            },
        ]
    }

    fn header(blocks: &[ContainerSealedBlock]) -> (ContainerSealedHeader, Vec<u8>) {
        let (index, index_checksum) = ContainerSealedHeader::encode_index(blocks);
        let header = ContainerSealedHeader {
            encrypted: true,
            image_bytes: 2 * ContainerSealedHeader::BLOCK_BYTES - 100,
            block_count: 2,
            index_offset: ContainerSealedHeader::DATA_OFFSET + 14,
            index_checksum,
        };
        (header, index)
    }

    #[test]
    fn round_trips_with_its_index() {
        let blocks = blocks();
        let (header, index) = header(&blocks);
        assert_eq!(ContainerSealedHeader::decode(&header.encode()), Ok(header));
        assert_eq!(header.decode_index(&index), Ok(blocks.clone()));
        assert!(blocks[0].is_hole());
        assert_eq!(blocks[1].check(b"a stored block"), Ok(()));
        assert_eq!(
            blocks[1].check(b"a stored blocK"),
            Err(ContainerFormatError::ChecksumMismatch)
        );
        assert_eq!(
            blocks[1].check(b"a stored"),
            Err(ContainerFormatError::InvalidField)
        );
    }

    #[test]
    fn refuses_damage_and_undefined_fields() {
        let (header, index) = header(&blocks());
        let bytes = header.encode();
        assert_eq!(
            ContainerSealedHeader::decode(&bytes[..63]),
            Err(ContainerFormatError::Truncated)
        );
        assert_eq!(
            ContainerSealedHeader::decode(&[0; 64]),
            Err(ContainerFormatError::BadMagic)
        );
        let mut flipped = bytes;
        flipped[20] ^= 1;
        assert_eq!(
            ContainerSealedHeader::decode(&flipped),
            Err(ContainerFormatError::ChecksumMismatch)
        );
        let reseal = |mut bytes: [u8; 64]| {
            let checksum = Crc32c::checksum(&bytes[..60]);
            bytes[60..].copy_from_slice(&checksum.to_le_bytes());
            ContainerSealedHeader::decode(&bytes)
        };
        let mut version = bytes;
        version[8] = 2;
        assert_eq!(
            reseal(version),
            Err(ContainerFormatError::UnsupportedVersion)
        );
        for (at, value) in [(10, 17), (11, 2), (13, 1), (50, 1), (24, 3), (33, 0)] {
            let mut bytes = bytes;
            bytes[at] = value;
            assert_eq!(
                reseal(bytes),
                Err(ContainerFormatError::InvalidField),
                "byte {at}"
            );
        }

        let mut damaged = index.clone();
        damaged[3] ^= 1;
        assert_eq!(
            header.decode_index(&damaged),
            Err(ContainerFormatError::ChecksumMismatch)
        );
        assert_eq!(
            header.decode_index(&index[..20]),
            Err(ContainerFormatError::Truncated)
        );
        // A block that reaches into the index, or a hole that names bytes,
        // is refused even with a matching checksum.
        for wrong in [
            ContainerSealedBlock {
                length: 15,
                ..blocks()[1]
            },
            ContainerSealedBlock {
                offset: 4096,
                ..blocks()[1]
            },
            ContainerSealedBlock {
                offset: 9,
                ..ContainerSealedBlock::default()
            },
        ] {
            let (header, index) = self::header(&[blocks()[0], wrong]);
            assert_eq!(
                header.decode_index(&index),
                Err(ContainerFormatError::InvalidField),
                "{wrong:?}"
            );
        }
    }
}
