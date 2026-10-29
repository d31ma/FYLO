use crate::container_bytes::{all_zero, read_u32, read_u64, write_u32, write_u64};
use crate::{ContainerFormatError, Crc32c};

/// Where a derived snapshot (a location table or prefix index) lives in a
/// `.fxdb` container: a run of consecutive extents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerSnapshotRef {
    /// First extent of the run.
    pub first_extent: u64,
    /// Extents in the run.
    pub extent_count: u32,
    /// Snapshot bytes across the run's payloads.
    pub length: u64,
    /// SHA-256 of the snapshot bytes.
    pub sha256: [u8; 32],
}

/// One of a collection's two checkpoint slots in a `.fxdb` container
/// (ADR 0016 section 3).
///
/// A checkpoint writes the slot that does not hold the newest checkpoint and
/// flushes. Recovery starts from the newest valid slot: it replays the
/// collection's segments from the recorded position and rebuilds a snapshot
/// whose bytes do not match the recorded hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerCheckpointSlot {
    /// Checkpoint number; each checkpoint writes the next one.
    pub checkpoint: u64,
    /// LSN of the checkpoint record.
    pub lsn: u64,
    /// Extent holding the checkpoint record.
    pub extent: u64,
    /// That extent's sequence number.
    pub extent_sequence: u64,
    /// Payload offset of the checkpoint record in that extent.
    pub offset: u64,
    /// The location snapshot the checkpoint wrote, if any.
    pub locations: Option<ContainerSnapshotRef>,
    /// The prefix-index snapshot the checkpoint wrote, if any.
    pub index: Option<ContainerSnapshotRef>,
    /// The current compression dictionary as (extent, dictionary id, length),
    /// if the collection has one.
    pub dictionary: Option<(u64, u32, u32)>,
}

impl ContainerCheckpointSlot {
    /// Encoded size of one slot.
    pub const ENCODED_BYTES: usize = 256;
    const MAGIC: [u8; 4] = *b"FXCP";
    const SNAPSHOT_BYTES: usize = 56;
    const LOCATIONS_OFFSET: usize = 44;
    const INDEX_OFFSET: usize = 100;
    const DICTIONARY_OFFSET: usize = 156;
    const CHECKED_BYTES: usize = Self::ENCODED_BYTES - 4;

    /// Encode the slot with its checksum.
    #[must_use]
    pub fn encode(&self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        bytes[0..4].copy_from_slice(&Self::MAGIC);
        write_u64(&mut bytes, 4, self.checkpoint);
        write_u64(&mut bytes, 12, self.lsn);
        write_u64(&mut bytes, 20, self.extent);
        write_u64(&mut bytes, 28, self.extent_sequence);
        write_u64(&mut bytes, 36, self.offset);
        Self::encode_snapshot(&mut bytes, Self::LOCATIONS_OFFSET, self.locations);
        Self::encode_snapshot(&mut bytes, Self::INDEX_OFFSET, self.index);
        if let Some((extent, id, length)) = self.dictionary {
            write_u64(&mut bytes, Self::DICTIONARY_OFFSET, extent);
            write_u32(&mut bytes, Self::DICTIONARY_OFFSET + 8, id);
            write_u32(&mut bytes, Self::DICTIONARY_OFFSET + 12, length);
        }
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        write_u32(&mut bytes, Self::CHECKED_BYTES, checksum);
        bytes
    }

    fn encode_snapshot(bytes: &mut [u8], at: usize, snapshot: Option<ContainerSnapshotRef>) {
        if let Some(snapshot) = snapshot {
            write_u64(bytes, at, snapshot.first_extent);
            write_u32(bytes, at + 8, snapshot.extent_count);
            write_u64(bytes, at + 16, snapshot.length);
            bytes[at + 24..at + Self::SNAPSHOT_BYTES].copy_from_slice(&snapshot.sha256);
        }
    }

    /// Decode and verify one slot.
    ///
    /// # Errors
    ///
    /// Returns the first defect: too few bytes, wrong magic, a checksum
    /// mismatch, a snapshot or dictionary with a zero extent but other
    /// fields set, or non-zero reserved bytes.
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
        if !all_zero(&bytes[Self::DICTIONARY_OFFSET + 16..Self::CHECKED_BYTES]) {
            return Err(ContainerFormatError::InvalidField);
        }
        let dictionary_bytes = &bytes[Self::DICTIONARY_OFFSET..Self::DICTIONARY_OFFSET + 16];
        let dictionary = match read_u64(dictionary_bytes, 0) {
            0 if all_zero(dictionary_bytes) => None,
            0 => return Err(ContainerFormatError::InvalidField),
            extent => Some((
                extent,
                read_u32(dictionary_bytes, 8),
                read_u32(dictionary_bytes, 12),
            )),
        };
        Ok(Self {
            checkpoint: read_u64(bytes, 4),
            lsn: read_u64(bytes, 12),
            extent: read_u64(bytes, 20),
            extent_sequence: read_u64(bytes, 28),
            offset: read_u64(bytes, 36),
            locations: Self::decode_snapshot(bytes, Self::LOCATIONS_OFFSET)?,
            index: Self::decode_snapshot(bytes, Self::INDEX_OFFSET)?,
            dictionary,
        })
    }

    fn decode_snapshot(
        bytes: &[u8],
        at: usize,
    ) -> Result<Option<ContainerSnapshotRef>, ContainerFormatError> {
        let field = &bytes[at..at + Self::SNAPSHOT_BYTES];
        if !all_zero(&field[12..16]) {
            return Err(ContainerFormatError::InvalidField);
        }
        let extent_count = read_u32(field, 8);
        if extent_count == 0 {
            return if all_zero(field) {
                Ok(None)
            } else {
                Err(ContainerFormatError::InvalidField)
            };
        }
        let first_extent = read_u64(field, 0);
        if first_extent == 0 {
            return Err(ContainerFormatError::InvalidField);
        }
        Ok(Some(ContainerSnapshotRef {
            first_extent,
            extent_count,
            length: read_u64(field, 16),
            sha256: field[24..Self::SNAPSHOT_BYTES]
                .try_into()
                .map_err(|_| ContainerFormatError::Truncated)?,
        }))
    }

    /// Read both slots (512 bytes, A then B) and take the valid one with the
    /// higher checkpoint number; A wins a tie.
    ///
    /// Returns the slot and whether it is A, or `None` when neither is valid
    /// (a collection that has never checkpointed replays from its first
    /// segment).
    #[must_use]
    pub fn newest(pair: &[u8]) -> Option<(Self, bool)> {
        let a = pair
            .get(..Self::ENCODED_BYTES)
            .and_then(|b| Self::decode(b).ok());
        let b = pair
            .get(Self::ENCODED_BYTES..2 * Self::ENCODED_BYTES)
            .and_then(|b| Self::decode(b).ok());
        match (a, b) {
            (Some(a), Some(b)) if b.checkpoint > a.checkpoint => Some((b, false)),
            (Some(a), _) => Some((a, true)),
            (None, Some(b)) => Some((b, false)),
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ContainerCheckpointSlot, ContainerSnapshotRef};
    use crate::ContainerFormatError;

    fn slot(checkpoint: u64) -> ContainerCheckpointSlot {
        ContainerCheckpointSlot {
            checkpoint,
            lsn: 1200,
            extent: 6,
            extent_sequence: 31,
            offset: 8192,
            locations: Some(ContainerSnapshotRef {
                first_extent: 9,
                extent_count: 1,
                length: 160_000,
                sha256: [3; 32],
            }),
            index: None,
            dictionary: Some((4, 1, 32 * 1024)),
        }
    }

    #[test]
    fn round_trips() {
        assert_eq!(
            ContainerCheckpointSlot::decode(&slot(1).encode()),
            Ok(slot(1))
        );
        let empty = ContainerCheckpointSlot {
            locations: None,
            dictionary: None,
            ..slot(2)
        };
        assert_eq!(ContainerCheckpointSlot::decode(&empty.encode()), Ok(empty));
    }

    #[test]
    fn picks_the_newest_slot() {
        let mut pair = [0_u8; 512];
        pair[..256].copy_from_slice(&slot(3).encode());
        pair[256..].copy_from_slice(&slot(2).encode());
        assert_eq!(
            ContainerCheckpointSlot::newest(&pair),
            Some((slot(3), true))
        );
        pair[10] ^= 1;
        assert_eq!(
            ContainerCheckpointSlot::newest(&pair),
            Some((slot(2), false))
        );
    }

    #[test]
    fn rejects_damage() {
        let mut bytes = slot(1).encode();
        bytes[60] ^= 2;
        assert_eq!(
            ContainerCheckpointSlot::decode(&bytes),
            Err(ContainerFormatError::ChecksumMismatch)
        );
    }
}
