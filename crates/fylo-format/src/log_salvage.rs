use crate::{LogFormatError, LogRecord, LogSegmentHeader, LogSegmentKind};

/// Every record that can still be read from a damaged document segment.
///
/// A scan stops at the first defect, because in the active segment that is
/// where an interrupted append ended. A salvage instead steps past a damaged
/// record to the next sync marker and keeps reading, so one bad sector costs
/// only the documents in the records it touches.
#[derive(Debug, Eq, PartialEq)]
pub struct LogSalvage {
    /// The segment's header.
    pub header: LogSegmentHeader,
    /// Every record that decoded, with the byte offset it starts at.
    pub records: Vec<(u64, LogRecord)>,
    /// Byte ranges, start inclusive and end exclusive, that held no
    /// readable record.
    pub damaged: Vec<(u64, u64)>,
}

impl LogSalvage {
    /// Salvage a whole document segment.
    ///
    /// # Errors
    ///
    /// Returns an error when the header is invalid, or `InvalidField` when
    /// the segment is a write-ahead log, whose frames carry no marker.
    pub fn segment(bytes: &[u8]) -> Result<Self, LogFormatError> {
        let header = LogSegmentHeader::decode(bytes)?;
        if !header.kind.marked() {
            return Err(LogFormatError::InvalidField);
        }
        let mut salvage = Self {
            header,
            records: Vec::new(),
            damaged: Vec::new(),
        };
        salvage.read_from(bytes, LogSegmentHeader::ENCODED_BYTES);
        Ok(salvage)
    }

    fn read_from(&mut self, bytes: &[u8], mut offset: usize) {
        while offset < bytes.len() {
            offset = match self.header.kind.unframe(&bytes[offset..]) {
                Ok((record, length)) => {
                    self.records.push((offset as u64, record));
                    offset + length
                }
                Err(_) => self.skip_damage(bytes, offset),
            };
        }
    }

    fn skip_damage(&mut self, bytes: &[u8], offset: usize) -> usize {
        let next = LogSegmentKind::next_marker(bytes, offset + 1).unwrap_or(bytes.len());
        self.damaged.push((offset as u64, next as u64));
        next
    }
}

#[cfg(test)]
mod tests {
    use super::LogSalvage;
    use crate::{LogFormatError, LogRecord, LogRecordKind, LogSegmentHeader, LogSegmentKind};

    fn segment(kind: LogSegmentKind, count: u64) -> (Vec<u8>, Vec<usize>) {
        let mut bytes = LogSegmentHeader {
            kind,
            sequence: 1,
            first_lsn: 1,
        }
        .encode()
        .to_vec();
        let mut starts = Vec::new();
        for lsn in 1..=count {
            starts.push(bytes.len());
            let record = LogRecord {
                lsn,
                kind: LogRecordKind::Commit,
                generation: 2,
                manifest: b"{}".to_vec(),
                blobs: vec![b'x'; 40],
            };
            bytes.extend(kind.frame(&record).unwrap());
        }
        (bytes, starts)
    }

    #[test]
    fn reads_past_a_damaged_record() {
        let (mut bytes, starts) = segment(LogSegmentKind::Documents, 3);
        bytes[starts[1] + 20] ^= 0xFF;
        let salvage = LogSalvage::segment(&bytes).unwrap();
        let lsns: Vec<u64> = salvage
            .records
            .iter()
            .map(|(_, record)| record.lsn)
            .collect();
        assert_eq!(lsns, vec![1, 3]);
        assert_eq!(salvage.damaged, vec![(starts[1] as u64, starts[2] as u64)]);
    }

    #[test]
    fn a_clean_segment_has_no_damage() {
        let (bytes, _) = segment(LogSegmentKind::CompactedDocuments, 4);
        let salvage = LogSalvage::segment(&bytes).unwrap();
        assert_eq!(salvage.records.len(), 4);
        assert!(salvage.damaged.is_empty());
    }

    #[test]
    fn every_flipped_byte_keeps_the_other_records() {
        let (bytes, starts) = segment(LogSegmentKind::Documents, 3);
        for index in starts[1]..starts[2] {
            let mut damaged = bytes.clone();
            damaged[index] ^= 0x5A;
            let salvage = LogSalvage::segment(&damaged).unwrap();
            let lsns: Vec<u64> = salvage
                .records
                .iter()
                .map(|(_, record)| record.lsn)
                .collect();
            assert!(lsns.contains(&1) && lsns.contains(&3), "byte {index}");
        }
    }

    /// Frame `record` as a document segment frame, then rewrite its kind
    /// byte to `code` with a checksum that still holds.
    fn reframed(record: &LogRecord, code: u8) -> Vec<u8> {
        let mut frame = LogSegmentKind::Documents.frame(record).unwrap();
        let body = LogSegmentKind::MARKER_BYTES + LogRecord::FRAME_BYTES;
        frame[body + 8] = code;
        let checksum = crate::Crc32c::checksum(&frame[body..]);
        frame[body - 4..body].copy_from_slice(&checksum.to_le_bytes());
        frame
    }

    #[test]
    fn a_schema_record_is_read_and_an_unknown_kind_is_damage() {
        let (mut bytes, _) = segment(LogSegmentKind::Documents, 1);
        let schema = LogRecord {
            lsn: 2,
            kind: LogRecordKind::Schema,
            generation: 2,
            manifest: br#"{"format":"fylo.schema-version.v1"}"#.to_vec(),
            blobs: b"{}".to_vec(),
        };
        let schema_start = bytes.len();
        bytes.extend(reframed(&schema, 5));
        let unknown_start = bytes.len();
        bytes.extend(reframed(
            &LogRecord {
                lsn: 3,
                ..schema.clone()
            },
            6,
        ));
        let salvage = LogSalvage::segment(&bytes).unwrap();
        assert_eq!(salvage.records.len(), 2);
        assert_eq!(salvage.records[1], (schema_start as u64, schema));
        assert_eq!(
            salvage.damaged,
            vec![(unknown_start as u64, bytes.len() as u64)]
        );
        let scan = crate::LogScan::segment(&bytes).unwrap();
        assert_eq!(scan.records.len(), 2);
        assert_eq!(scan.valid_length, unknown_start as u64);
        assert_eq!(scan.stopped, Some(LogFormatError::InvalidField));
    }

    #[test]
    fn a_write_ahead_log_cannot_be_salvaged() {
        let (bytes, _) = segment(LogSegmentKind::WriteAheadLog, 1);
        assert_eq!(
            LogSalvage::segment(&bytes),
            Err(LogFormatError::InvalidField)
        );
    }
}
