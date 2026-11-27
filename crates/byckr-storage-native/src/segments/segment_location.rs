use super::LocationState;
use crate::{NativeStorageError, NativeStorageErrorCode};

/// Where the newest entry for one document sits.
///
/// Encoded as a fixed 72-byte row: the identifier zero-padded to 40 bytes,
/// then segment number, record offset, record length, entry index, state and
/// sequence number, little endian.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SegmentLocation {
    pub(crate) id: String,
    pub(crate) segment: u64,
    pub(crate) offset: u64,
    pub(crate) length: u32,
    pub(crate) entry: u16,
    pub(crate) state: LocationState,
    pub(crate) lsn: u64,
}

impl SegmentLocation {
    pub(crate) const ENCODED_BYTES: usize = 72;
    const ID_BYTES: usize = 40;

    pub(crate) fn encode(&self) -> Result<[u8; Self::ENCODED_BYTES], NativeStorageError> {
        let id = self.id.as_bytes();
        if id.is_empty() || id.len() > Self::ID_BYTES || id.contains(&0) {
            return Err(Self::corrupt("segment location identifier does not fit"));
        }
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        bytes[..id.len()].copy_from_slice(id);
        bytes[40..48].copy_from_slice(&self.segment.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.offset.to_le_bytes());
        bytes[56..60].copy_from_slice(&self.length.to_le_bytes());
        bytes[60..62].copy_from_slice(&self.entry.to_le_bytes());
        bytes[62] = self.state.code();
        bytes[64..72].copy_from_slice(&self.lsn.to_le_bytes());
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, NativeStorageError> {
        let bytes = bytes
            .get(..Self::ENCODED_BYTES)
            .ok_or_else(|| Self::corrupt("segment location row is truncated"))?;
        let id_length = bytes[..Self::ID_BYTES]
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(Self::ID_BYTES);
        let id = std::str::from_utf8(&bytes[..id_length])
            .map_err(|_| Self::corrupt("segment location identifier is not UTF-8"))?;
        if id.is_empty() || bytes[63] != 0 {
            return Err(Self::corrupt("segment location row is malformed"));
        }
        Ok(Self {
            id: id.to_owned(),
            segment: Self::u64_at(bytes, 40),
            offset: Self::u64_at(bytes, 48),
            length: u32::from_le_bytes([bytes[56], bytes[57], bytes[58], bytes[59]]),
            entry: u16::from_le_bytes([bytes[60], bytes[61]]),
            state: LocationState::from_code(bytes[62])?,
            lsn: Self::u64_at(bytes, 64),
        })
    }

    /// Decode a whole table of rows.
    pub(crate) fn decode_all(bytes: &[u8]) -> Result<Vec<Self>, NativeStorageError> {
        if !bytes.len().is_multiple_of(Self::ENCODED_BYTES) {
            return Err(Self::corrupt("segment location table has a partial row"));
        }
        bytes
            .chunks(Self::ENCODED_BYTES)
            .map(Self::decode)
            .collect()
    }

    fn u64_at(bytes: &[u8], start: usize) -> u64 {
        let mut value = [0_u8; 8];
        value.copy_from_slice(&bytes[start..start + 8]);
        u64::from_le_bytes(value)
    }

    fn corrupt(message: &str) -> NativeStorageError {
        NativeStorageError::new(NativeStorageErrorCode::CorruptMetadata, message)
    }
}

#[cfg(test)]
mod tests {
    use super::{LocationState, SegmentLocation};

    fn location(id: &str) -> SegmentLocation {
        SegmentLocation {
            id: id.into(),
            segment: 2,
            offset: 512,
            length: 300,
            entry: 4,
            state: LocationState::Deleted,
            lsn: 99,
        }
    }

    #[test]
    fn round_trips() {
        let row = location("4UUB32VGUDW").encode().unwrap();
        assert_eq!(
            SegmentLocation::decode(&row).unwrap(),
            location("4UUB32VGUDW")
        );
    }

    #[test]
    fn a_long_identifier_round_trips() {
        let id = "4UUB32VGUDW-4UUB32VGUDW-4UUB32VGUDW";
        let row = location(id).encode().unwrap();
        assert_eq!(SegmentLocation::decode(&row).unwrap().id, id);
    }

    #[test]
    fn refuses_an_identifier_that_does_not_fit() {
        assert!(location(&"A".repeat(41)).encode().is_err());
    }

    #[test]
    fn refuses_a_partial_table() {
        assert!(SegmentLocation::decode_all(&[0_u8; 71]).is_err());
    }
}
