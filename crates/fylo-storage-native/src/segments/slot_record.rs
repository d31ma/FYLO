use fylo_format::Crc32c;

/// One generation slot of a segment collection (ADR 0015 section 6).
///
/// A slot holds the published generation, whether a write is in progress,
/// where the active segment's committed bytes end, and how long the
/// location log was when the generation was published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SlotRecord {
    pub(crate) generation: u64,
    pub(crate) writing: bool,
    pub(crate) segment: u64,
    pub(crate) length: u64,
    pub(crate) next_lsn: u64,
    pub(crate) records: u64,
    pub(crate) locations_length: u64,
    /// Bumped whenever the location snapshot is rewritten, so a cached
    /// table knows when it can only be extended and when it must reload.
    pub(crate) table_epoch: u32,
}

impl SlotRecord {
    pub(crate) const ENCODED_BYTES: usize = 64;
    const MAGIC: [u8; 4] = *b"FYSL";
    const VERSION: u8 = 1;
    const CHECKED_BYTES: usize = 60;

    pub(crate) fn encode(&self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        bytes[0..4].copy_from_slice(&Self::MAGIC);
        bytes[4] = Self::VERSION;
        bytes[5] = u8::from(self.writing);
        let fields = [
            self.generation,
            self.segment,
            self.length,
            self.next_lsn,
            self.records,
            self.locations_length,
        ];
        for (index, value) in fields.iter().enumerate() {
            let start = 8 + index * 8;
            bytes[start..start + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[56..60].copy_from_slice(&self.table_epoch.to_le_bytes());
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        bytes[60..64].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    /// Decode a slot, or `None` when it was never written or is torn.
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..Self::ENCODED_BYTES)?;
        let checksum = u32::from_le_bytes([bytes[60], bytes[61], bytes[62], bytes[63]]);
        let valid = bytes[0..4] == Self::MAGIC
            && bytes[4] == Self::VERSION
            && bytes[5] <= 1
            && Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]) == checksum;
        if !valid {
            return None;
        }
        Some(Self {
            generation: Self::field(bytes, 0),
            writing: bytes[5] == 1,
            segment: Self::field(bytes, 1),
            length: Self::field(bytes, 2),
            next_lsn: Self::field(bytes, 3),
            records: Self::field(bytes, 4),
            locations_length: Self::field(bytes, 5),
            table_epoch: u32::from_le_bytes([bytes[56], bytes[57], bytes[58], bytes[59]]),
        })
    }

    fn field(bytes: &[u8], index: usize) -> u64 {
        let start = 8 + index * 8;
        let mut value = [0_u8; 8];
        value.copy_from_slice(&bytes[start..start + 8]);
        u64::from_le_bytes(value)
    }
}

#[cfg(test)]
mod tests {
    use super::SlotRecord;

    fn slot() -> SlotRecord {
        SlotRecord {
            generation: 8,
            writing: true,
            segment: 3,
            length: 4_096,
            next_lsn: 77,
            records: 12,
            locations_length: 720,
            table_epoch: 5,
        }
    }

    #[test]
    fn round_trips() {
        assert_eq!(SlotRecord::decode(&slot().encode()), Some(slot()));
    }

    #[test]
    fn a_torn_slot_is_invalid() {
        let mut bytes = slot().encode();
        bytes[20] ^= 1;
        assert_eq!(SlotRecord::decode(&bytes), None);
    }

    #[test]
    fn an_unwritten_slot_is_invalid() {
        assert_eq!(SlotRecord::decode(&[0_u8; 64]), None);
    }
}
