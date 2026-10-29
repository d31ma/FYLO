use crate::container_bytes::{all_zero, read_u32, read_u64, write_u32, write_u64};
use crate::{ContainerFormatError, Crc32c};

/// One of a collection's two generation slots in a `.fxdb` container
/// (ADR 0016 section 3; ADR 0015 section 6).
///
/// A publication overwrites, in place and without a flush, the slot that does
/// not hold the newest valid generation. A torn slot fails its checksum, so a
/// reader sees the previous generation and the committed-length check sends
/// it to recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerGenerationSlot {
    /// Collection generation.
    pub generation: u64,
    /// Whether a mutation is between its `writing` and `stable` publications.
    pub writing: bool,
    /// The collection's active segment extent.
    pub active_extent: u64,
    /// That extent's sequence number.
    pub active_sequence: u64,
    /// Committed payload bytes in the active extent.
    pub committed_length: u64,
    /// The next log sequence number to assign.
    pub next_lsn: u64,
    /// Records committed in the collection.
    pub record_count: u64,
    /// Bumped whenever the collection's location snapshot is rewritten other
    /// than by appending, so a reader's cached location table knows when it
    /// can only be extended and when it must be loaded again.
    pub table_epoch: u32,
}

impl ContainerGenerationSlot {
    /// Encoded size of one slot.
    pub const ENCODED_BYTES: usize = 64;
    const MAGIC: [u8; 4] = *b"FXGS";
    const CHECKED_BYTES: usize = Self::ENCODED_BYTES - 4;

    /// Encode the slot with its checksum.
    #[must_use]
    pub fn encode(&self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        bytes[0..4].copy_from_slice(&Self::MAGIC);
        write_u64(&mut bytes, 4, self.generation);
        bytes[12] = u8::from(self.writing);
        write_u64(&mut bytes, 16, self.active_extent);
        write_u64(&mut bytes, 24, self.active_sequence);
        write_u64(&mut bytes, 32, self.committed_length);
        write_u64(&mut bytes, 40, self.next_lsn);
        write_u64(&mut bytes, 48, self.record_count);
        write_u32(&mut bytes, 56, self.table_epoch);
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        write_u32(&mut bytes, Self::CHECKED_BYTES, checksum);
        bytes
    }

    /// Decode and verify one slot.
    ///
    /// # Errors
    ///
    /// Returns the first defect: too few bytes, wrong magic, a checksum
    /// mismatch, an unknown state or non-zero reserved bytes.
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
        let writing = match bytes[12] {
            0 => false,
            1 => true,
            _ => return Err(ContainerFormatError::InvalidField),
        };
        if !all_zero(&bytes[13..16]) {
            return Err(ContainerFormatError::InvalidField);
        }
        Ok(Self {
            generation: read_u64(bytes, 4),
            writing,
            active_extent: read_u64(bytes, 16),
            active_sequence: read_u64(bytes, 24),
            committed_length: read_u64(bytes, 32),
            next_lsn: read_u64(bytes, 40),
            record_count: read_u64(bytes, 48),
            table_epoch: read_u32(bytes, 56),
        })
    }

    /// Read both slots (128 bytes, A then B) and take the valid one with the
    /// higher generation; A wins a tie.
    ///
    /// Returns the slot and whether it is A, so the next publication
    /// overwrites the other one, or `None` when neither slot is valid.
    #[must_use]
    pub fn newest(pair: &[u8]) -> Option<(Self, bool)> {
        let a = pair
            .get(..Self::ENCODED_BYTES)
            .and_then(|b| Self::decode(b).ok());
        let b = pair
            .get(Self::ENCODED_BYTES..2 * Self::ENCODED_BYTES)
            .and_then(|b| Self::decode(b).ok());
        match (a, b) {
            (Some(a), Some(b)) if b.generation > a.generation => Some((b, false)),
            (Some(a), _) => Some((a, true)),
            (None, Some(b)) => Some((b, false)),
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerGenerationSlot;
    use crate::ContainerFormatError;

    fn slot(generation: u64) -> ContainerGenerationSlot {
        ContainerGenerationSlot {
            generation,
            writing: generation % 2 == 1,
            active_extent: 5,
            active_sequence: 12,
            committed_length: 70_000,
            next_lsn: 900,
            record_count: 899,
            table_epoch: 4,
        }
    }

    #[test]
    fn round_trips() {
        assert_eq!(
            ContainerGenerationSlot::decode(&slot(7).encode()),
            Ok(slot(7))
        );
    }

    #[test]
    fn picks_the_newest_slot_and_survives_a_torn_one() {
        let mut pair = [0_u8; 128];
        pair[..64].copy_from_slice(&slot(8).encode());
        pair[64..].copy_from_slice(&slot(9).encode());
        assert_eq!(
            ContainerGenerationSlot::newest(&pair),
            Some((slot(9), false))
        );

        pair[100] ^= 0xFF;
        assert_eq!(
            ContainerGenerationSlot::newest(&pair),
            Some((slot(8), true))
        );

        assert_eq!(ContainerGenerationSlot::newest(&[0_u8; 128]), None);
    }

    #[test]
    fn rejects_damage() {
        let mut bytes = slot(2).encode();
        bytes[30] ^= 1;
        assert_eq!(
            ContainerGenerationSlot::decode(&bytes),
            Err(ContainerFormatError::ChecksumMismatch)
        );
    }
}
