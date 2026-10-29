use crate::container_bytes::{
    all_zero, read_u16, read_u32, read_u64, write_u16, write_u32, write_u64,
};
use crate::{ContainerFormatError, Crc32c};

/// The superblock of a `.fxdb` container (ADR 0016 section 3).
///
/// Two copies sit at offsets 0 and [`Self::ENCODED_BYTES`]. A checkpoint
/// writes the copy that does not hold the newest valid generation and flushes;
/// a reader takes the valid copy with the higher generation
/// ([`Self::newest`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerSuperblock {
    /// Superblock generation; each checkpoint writes the next one.
    pub generation: u64,
    /// Extent size as a power of two.
    pub extent_shift: u8,
    /// Extent slots in the file. Slot 0 is the superblock region.
    pub extent_count: u64,
    /// The next extent sequence number to hand out; never reused.
    pub next_extent_sequence: u64,
    /// First collection-directory extent.
    pub directory_extent: u64,
    /// First extent of the current extent-map copy.
    pub map_first_extent: u64,
    /// Extents in the current extent-map copy.
    pub map_extent_count: u32,
    /// Compatible feature flags: unknown bits are ignored.
    pub compat: u32,
    /// Read-only-compatible feature flags: unknown bits allow reading only.
    pub read_only_compat: u32,
    /// Incompatible feature flags: unknown bits refuse the file.
    pub incompat: u32,
    /// HKDF salt for the file key; zero when the file is not encrypted.
    pub key_salt: [u8; 32],
    /// Key-check value that tells a wrong key from damage; zero when the file
    /// is not encrypted.
    pub key_check: [u8; 16],
}

impl ContainerSuperblock {
    /// Encoded size of one copy.
    pub const ENCODED_BYTES: usize = 4096;
    /// Current major format version.
    pub const VERSION: u16 = 1;
    /// PNG-style magic: a high bit, the name, then CR LF and EOF bytes, so a
    /// text-mode transfer or a 7-bit channel breaks it visibly.
    pub const MAGIC: [u8; 8] = [0x89, b'F', b'X', b'D', b'B', b'\r', b'\n', 0x1A];
    /// Smallest extent: 1 MiB.
    pub const MIN_EXTENT_SHIFT: u8 = 20;
    /// Largest extent: 64 MiB.
    pub const MAX_EXTENT_SHIFT: u8 = 26;
    /// Default extent: 8 MiB.
    pub const DEFAULT_EXTENT_SHIFT: u8 = 23;

    /// Records are sealed with AES-256-GCM (ADR 0016 section 9).
    pub const INCOMPAT_ENCRYPTED: u32 = 1 << 0;
    /// The file is a read-only pack with block-compressed extents.
    pub const INCOMPAT_SEALED: u32 = 1 << 1;
    /// Some frames are zstd-compressed.
    pub const INCOMPAT_CODEC_ZSTD: u32 = 1 << 2;
    /// Some frames are LZ4-compressed.
    pub const INCOMPAT_CODEC_LZ4: u32 = 1 << 3;
    const KNOWN_INCOMPAT: u32 = Self::INCOMPAT_ENCRYPTED
        | Self::INCOMPAT_SEALED
        | Self::INCOMPAT_CODEC_ZSTD
        | Self::INCOMPAT_CODEC_LZ4;
    /// Collections carry schema-version records and a schema registry
    /// (ADR 0017). Reserved: this build does not know it yet, so it opens
    /// such a file read-only rather than write unvalidated documents.
    pub const READ_ONLY_COMPAT_SCHEMAS: u32 = 1 << 0;
    const KNOWN_READ_ONLY_COMPAT: u32 = 0;
    const CHECKED_BYTES: usize = Self::ENCODED_BYTES - 4;

    /// Bytes in one extent.
    #[must_use]
    pub const fn extent_bytes(&self) -> u64 {
        1 << self.extent_shift
    }

    /// Byte offset of extent `index`, or `None` past the end of the file or
    /// on overflow.
    #[must_use]
    pub fn extent_offset(&self, index: u64) -> Option<u64> {
        if index >= self.extent_count {
            return None;
        }
        index.checked_mul(self.extent_bytes())
    }

    /// Whether the file is encrypted.
    #[must_use]
    pub const fn is_encrypted(&self) -> bool {
        self.incompat & Self::INCOMPAT_ENCRYPTED != 0
    }

    /// Whether the file is a sealed, read-only pack.
    #[must_use]
    pub const fn is_sealed(&self) -> bool {
        self.incompat & Self::INCOMPAT_SEALED != 0
    }

    /// Whether this build may only read the file: it sets a
    /// read-only-compatible feature this build does not know, or it is sealed.
    #[must_use]
    pub const fn requires_read_only(&self) -> bool {
        self.read_only_compat & !Self::KNOWN_READ_ONLY_COMPAT != 0 || self.is_sealed()
    }

    /// Encode one superblock copy with its checksum.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![0_u8; Self::ENCODED_BYTES];
        bytes[0..8].copy_from_slice(&Self::MAGIC);
        write_u16(&mut bytes, 8, Self::VERSION);
        write_u32(&mut bytes, 12, self.compat);
        write_u32(&mut bytes, 16, self.read_only_compat);
        write_u32(&mut bytes, 20, self.incompat);
        bytes[24] = self.extent_shift;
        write_u64(&mut bytes, 32, self.generation);
        write_u64(&mut bytes, 40, self.extent_count);
        write_u64(&mut bytes, 48, self.next_extent_sequence);
        write_u64(&mut bytes, 56, self.directory_extent);
        write_u64(&mut bytes, 64, self.map_first_extent);
        write_u32(&mut bytes, 72, self.map_extent_count);
        bytes[80..112].copy_from_slice(&self.key_salt);
        bytes[112..128].copy_from_slice(&self.key_check);
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        write_u32(&mut bytes, Self::CHECKED_BYTES, checksum);
        bytes
    }

    /// Decode and verify one superblock copy.
    ///
    /// # Errors
    ///
    /// Returns the first defect found: too few bytes, wrong magic, a checksum
    /// mismatch, an unknown major version, an unknown incompatible feature, or
    /// a field the format does not allow.
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
        let incompat = read_u32(bytes, 20);
        if incompat & !Self::KNOWN_INCOMPAT != 0 {
            return Err(ContainerFormatError::IncompatibleFeature);
        }
        let superblock = Self {
            generation: read_u64(bytes, 32),
            extent_shift: bytes[24],
            extent_count: read_u64(bytes, 40),
            next_extent_sequence: read_u64(bytes, 48),
            directory_extent: read_u64(bytes, 56),
            map_first_extent: read_u64(bytes, 64),
            map_extent_count: read_u32(bytes, 72),
            compat: read_u32(bytes, 12),
            read_only_compat: read_u32(bytes, 16),
            incompat,
            key_salt: bytes[80..112]
                .try_into()
                .map_err(|_| ContainerFormatError::Truncated)?,
            key_check: bytes[112..128]
                .try_into()
                .map_err(|_| ContainerFormatError::Truncated)?,
        };
        let reserved_clear = all_zero(&bytes[10..12])
            && all_zero(&bytes[25..32])
            && all_zero(&bytes[76..80])
            && all_zero(&bytes[128..Self::CHECKED_BYTES]);
        let encryption_consistent = superblock.is_encrypted()
            || (all_zero(&superblock.key_salt) && all_zero(&superblock.key_check));
        let extents_consistent = (Self::MIN_EXTENT_SHIFT..=Self::MAX_EXTENT_SHIFT)
            .contains(&superblock.extent_shift)
            && superblock.directory_extent >= 1
            && superblock.directory_extent < superblock.extent_count
            && (superblock.map_extent_count == 0
                || (superblock.map_first_extent >= 1
                    && superblock
                        .map_first_extent
                        .checked_add(u64::from(superblock.map_extent_count))
                        .is_some_and(|end| end <= superblock.extent_count)))
            && superblock
                .extent_count
                .checked_mul(superblock.extent_bytes())
                .is_some();
        if !(reserved_clear && encryption_consistent && extents_consistent) {
            return Err(ContainerFormatError::InvalidField);
        }
        Ok(superblock)
    }

    /// Choose the superblock to trust from the two copies.
    ///
    /// The valid copy with the higher generation wins, and `copy_a` wins a
    /// tie. A copy that is intact but too new for this build (an unknown
    /// version or incompatible feature) refuses the whole file rather than
    /// falling back to the older copy, which would silently roll back what
    /// the newer writer did.
    ///
    /// Returns the superblock and whether it came from copy A, so a writer
    /// knows to overwrite the other copy next.
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedVersion` or `IncompatibleFeature` when either copy
    /// is too new, and otherwise copy A's error when neither copy is valid.
    pub fn newest(copy_a: &[u8], copy_b: &[u8]) -> Result<(Self, bool), ContainerFormatError> {
        let a = Self::decode(copy_a);
        let b = Self::decode(copy_b);
        for result in [&a, &b] {
            if let Err(
                error @ (ContainerFormatError::UnsupportedVersion
                | ContainerFormatError::IncompatibleFeature),
            ) = result
            {
                return Err(*error);
            }
        }
        match (a, b) {
            (Ok(a), Ok(b)) if b.generation > a.generation => Ok((b, false)),
            (Ok(a), _) => Ok((a, true)),
            (Err(_), Ok(b)) => Ok((b, false)),
            (Err(error), Err(_)) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ContainerFormatError, ContainerSuperblock};
    use crate::Crc32c;

    fn sample(generation: u64) -> ContainerSuperblock {
        ContainerSuperblock {
            generation,
            extent_shift: ContainerSuperblock::DEFAULT_EXTENT_SHIFT,
            extent_count: 16,
            next_extent_sequence: 9,
            directory_extent: 1,
            map_first_extent: 2,
            map_extent_count: 1,
            compat: 0,
            read_only_compat: 0,
            incompat: ContainerSuperblock::INCOMPAT_CODEC_ZSTD,
            key_salt: [0; 32],
            key_check: [0; 16],
        }
    }

    fn resealed(mut bytes: Vec<u8>) -> Vec<u8> {
        let end = ContainerSuperblock::ENCODED_BYTES - 4;
        let checksum = Crc32c::checksum(&bytes[..end]);
        bytes[end..].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    #[test]
    fn round_trips() {
        let superblock = sample(3);
        assert_eq!(
            ContainerSuperblock::decode(&superblock.encode()),
            Ok(superblock)
        );
    }

    #[test]
    fn round_trips_an_encrypted_superblock() {
        let mut superblock = sample(3);
        superblock.incompat |= ContainerSuperblock::INCOMPAT_ENCRYPTED;
        superblock.key_salt = [7; 32];
        superblock.key_check = [9; 16];
        let decoded = ContainerSuperblock::decode(&superblock.encode()).unwrap();
        assert!(decoded.is_encrypted());
        assert_eq!(decoded, superblock);
    }

    #[test]
    fn computes_extent_offsets() {
        let superblock = sample(1);
        assert_eq!(superblock.extent_bytes(), 8 * 1024 * 1024);
        assert_eq!(superblock.extent_offset(2), Some(16 * 1024 * 1024));
        assert_eq!(superblock.extent_offset(16), None);
    }

    #[test]
    fn rejects_damage() {
        let bytes = sample(1).encode();
        let mut flipped = bytes.clone();
        flipped[40] ^= 1;
        assert_eq!(
            ContainerSuperblock::decode(&flipped),
            Err(ContainerFormatError::ChecksumMismatch)
        );
        assert_eq!(
            ContainerSuperblock::decode(&bytes[..100]),
            Err(ContainerFormatError::Truncated)
        );
        assert_eq!(
            ContainerSuperblock::decode(&[0_u8; 4096]),
            Err(ContainerFormatError::BadMagic)
        );
    }

    #[test]
    fn refuses_unknown_incompatible_features_but_reads_unknown_compatible_ones() {
        let mut bytes = sample(1).encode();
        bytes[20..24].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        assert_eq!(
            ContainerSuperblock::decode(&resealed(bytes)),
            Err(ContainerFormatError::IncompatibleFeature)
        );

        let mut compat = sample(1);
        compat.compat = 1 << 5;
        assert!(
            !ContainerSuperblock::decode(&compat.encode())
                .unwrap()
                .requires_read_only()
        );

        let mut read_only = sample(1);
        read_only.read_only_compat = 1 << 5;
        assert!(
            ContainerSuperblock::decode(&read_only.encode())
                .unwrap()
                .requires_read_only()
        );
    }

    #[test]
    fn rejects_inconsistent_fields() {
        let mut shift = sample(1);
        shift.extent_shift = 19;
        assert_eq!(
            ContainerSuperblock::decode(&shift.encode()),
            Err(ContainerFormatError::InvalidField)
        );

        let mut key_without_flag = sample(1);
        key_without_flag.key_salt = [1; 32];
        assert_eq!(
            ContainerSuperblock::decode(&key_without_flag.encode()),
            Err(ContainerFormatError::InvalidField)
        );

        let mut map_past_end = sample(1);
        map_past_end.map_first_extent = 16;
        assert_eq!(
            ContainerSuperblock::decode(&map_past_end.encode()),
            Err(ContainerFormatError::InvalidField)
        );
    }

    #[test]
    fn picks_the_newest_valid_copy() {
        let older = sample(4).encode();
        let newer = sample(5).encode();
        assert_eq!(
            ContainerSuperblock::newest(&older, &newer)
                .unwrap()
                .0
                .generation,
            5
        );
        assert_eq!(
            ContainerSuperblock::newest(&newer, &older)
                .unwrap()
                .0
                .generation,
            5
        );

        // A torn write of the newer copy falls back to the older one.
        let mut torn = newer.clone();
        torn[2000..].fill(0);
        let (chosen, from_a) = ContainerSuperblock::newest(&older, &torn).unwrap();
        assert_eq!((chosen.generation, from_a), (4, true));

        assert_eq!(
            ContainerSuperblock::newest(&torn, &[0; 4096]),
            Err(ContainerFormatError::ChecksumMismatch)
        );
    }

    #[test]
    fn does_not_roll_back_past_a_newer_writer() {
        let mut too_new = sample(9).encode();
        too_new[20..24].copy_from_slice(&(1_u32 << 30).to_le_bytes());
        let too_new = resealed(too_new);
        assert_eq!(
            ContainerSuperblock::newest(&sample(8).encode(), &too_new),
            Err(ContainerFormatError::IncompatibleFeature)
        );
    }
}
