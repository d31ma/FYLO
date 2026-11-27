use crate::{Crc32c, LogFormatError, LogSegmentKind};

/// The 32-byte header that opens every collection log and document segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogSegmentHeader {
    /// Which segment family this is, carried by the magic and the flags.
    pub kind: LogSegmentKind,
    /// The segment's identity: a random number for a log segment, the
    /// segment number for a document segment.
    pub sequence: u64,
    /// Log sequence number of the segment's first record.
    pub first_lsn: u64,
}

impl LogSegmentHeader {
    /// Encoded size in bytes.
    pub const ENCODED_BYTES: usize = 32;
    /// Current segment format version.
    pub const VERSION: u16 = 1;
    const CHECKED_BYTES: usize = 28;

    /// Encode the header with its checksum.
    #[must_use]
    pub fn encode(&self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0_u8; Self::ENCODED_BYTES];
        bytes[0..8].copy_from_slice(&self.kind.magic());
        bytes[8..10].copy_from_slice(&Self::VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&self.kind.flags().to_le_bytes());
        bytes[12..20].copy_from_slice(&self.sequence.to_le_bytes());
        bytes[20..28].copy_from_slice(&self.first_lsn.to_le_bytes());
        let checksum = Crc32c::checksum(&bytes[..Self::CHECKED_BYTES]);
        bytes[28..32].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    /// Decode and verify a header from the start of a segment.
    ///
    /// # Errors
    ///
    /// Returns the first defect found: too few bytes, wrong magic, a checksum
    /// mismatch, an unknown version, or flags the magic does not allow.
    pub fn decode(bytes: &[u8]) -> Result<Self, LogFormatError> {
        let header = bytes
            .get(..Self::ENCODED_BYTES)
            .ok_or(LogFormatError::Truncated)?;
        if !LogSegmentKind::known_magic(&header[0..8]) {
            return Err(LogFormatError::BadMagic);
        }
        if Crc32c::checksum(&header[..Self::CHECKED_BYTES]) != read_u32(&header[28..32]) {
            return Err(LogFormatError::ChecksumMismatch);
        }
        if read_u16(&header[8..10]) != Self::VERSION {
            return Err(LogFormatError::UnsupportedVersion);
        }
        let kind = LogSegmentKind::from_header(&header[0..8], read_u16(&header[10..12]))?;
        Ok(Self {
            kind,
            sequence: read_u64(&header[12..20]),
            first_lsn: read_u64(&header[20..28]),
        })
    }
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_u64(bytes: &[u8]) -> u64 {
    let mut value = [0_u8; 8];
    value.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(value)
}

#[cfg(test)]
mod tests {
    use super::{LogFormatError, LogSegmentHeader, LogSegmentKind};

    #[test]
    fn round_trips() {
        let header = LogSegmentHeader {
            kind: LogSegmentKind::WriteAheadLog,
            sequence: 7,
            first_lsn: 41,
        };
        assert_eq!(LogSegmentHeader::decode(&header.encode()), Ok(header));
    }

    #[test]
    fn rejects_a_flipped_bit() {
        let mut bytes = LogSegmentHeader {
            kind: LogSegmentKind::WriteAheadLog,
            sequence: 1,
            first_lsn: 1,
        }
        .encode();
        bytes[14] ^= 1;
        assert_eq!(
            LogSegmentHeader::decode(&bytes),
            Err(LogFormatError::ChecksumMismatch)
        );
    }

    #[test]
    fn a_compacted_document_header_round_trips() {
        let header = LogSegmentHeader {
            kind: LogSegmentKind::CompactedDocuments,
            sequence: 9,
            first_lsn: 3,
        };
        let bytes = header.encode();
        assert_eq!(&bytes[0..8], b"FYLOSEG\0");
        assert_eq!(LogSegmentHeader::decode(&bytes), Ok(header));
    }

    #[test]
    fn rejects_a_short_header() {
        assert_eq!(
            LogSegmentHeader::decode(b"FYLOLOG\0"),
            Err(LogFormatError::Truncated)
        );
    }

    #[test]
    fn rejects_foreign_bytes() {
        assert_eq!(
            LogSegmentHeader::decode(&[0_u8; 32]),
            Err(LogFormatError::BadMagic)
        );
    }
}
