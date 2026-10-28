use crate::{LogFormatError, LogRecord, LogSegmentHeader};

/// The valid records of one segment, read front to back.
///
/// A scan stops at the first record that fails to decode or breaks the
/// sequence. That point is the end of the durable log: an interrupted append
/// leaves a torn tail there, and no acknowledged commit lies beyond it.
#[derive(Debug, Eq, PartialEq)]
pub struct LogScan {
    /// The segment's header.
    pub header: LogSegmentHeader,
    /// Every valid record with the byte offset it starts at.
    pub records: Vec<(u64, LogRecord)>,
    /// Byte length of the valid prefix, header included.
    pub valid_length: u64,
    /// Why the scan stopped before the end, if it did.
    pub stopped: Option<LogFormatError>,
}

impl LogScan {
    /// Scan a whole segment.
    ///
    /// # Errors
    ///
    /// Returns an error only when the header itself is invalid. A defect
    /// after the header ends the scan and is reported in [`Self::stopped`].
    pub fn segment(bytes: &[u8]) -> Result<Self, LogFormatError> {
        let header = LogSegmentHeader::decode(bytes)?;
        let mut scan = Self {
            header,
            records: Vec::new(),
            valid_length: LogSegmentHeader::ENCODED_BYTES as u64,
            stopped: None,
        };
        scan.extend(bytes, header.first_lsn);
        Ok(scan)
    }

    /// Continue a scan over bytes appended after `valid_length`.
    ///
    /// `bytes` is the whole segment; only the suffix past the current valid
    /// length is read.
    pub fn resume(&mut self, bytes: &[u8]) {
        let next = self
            .records
            .last()
            .map_or(self.header.first_lsn, |(_, record)| record.lsn + 1);
        self.stopped = None;
        self.extend(bytes, next);
    }

    fn extend(&mut self, bytes: &[u8], mut expected: u64) {
        let mut offset = usize::try_from(self.valid_length).unwrap_or(usize::MAX);
        while offset < bytes.len() {
            match LogRecord::decode(&bytes[offset..]) {
                Ok((record, _)) if record.lsn != expected => {
                    self.stopped = Some(LogFormatError::SequenceGap);
                    return;
                }
                Ok((record, length)) => {
                    self.records.push((offset as u64, record));
                    offset += length;
                    self.valid_length = offset as u64;
                    expected += 1;
                }
                Err(error) => {
                    self.stopped = Some(error);
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LogScan;
    use crate::{LogFormatError, LogRecord, LogRecordKind, LogSegmentHeader};

    fn segment(lsns: &[u64]) -> Vec<u8> {
        let mut bytes = LogSegmentHeader {
            sequence: 3,
            first_lsn: 10,
        }
        .encode()
        .to_vec();
        for lsn in lsns {
            let record = LogRecord {
                lsn: *lsn,
                kind: LogRecordKind::Commit,
                generation: 2,
                manifest: b"{}".to_vec(),
                blobs: Vec::new(),
            };
            bytes.extend(record.encode().unwrap());
        }
        bytes
    }

    #[test]
    fn reads_every_record_of_a_clean_segment() {
        let bytes = segment(&[10, 11, 12]);
        let scan = LogScan::segment(&bytes).unwrap();
        assert_eq!(scan.records.len(), 3);
        assert_eq!(scan.valid_length, bytes.len() as u64);
        assert_eq!(scan.stopped, None);
    }

    #[test]
    fn stops_at_a_torn_tail() {
        let bytes = segment(&[10, 11]);
        let torn = &bytes[..bytes.len() - 3];
        let scan = LogScan::segment(torn).unwrap();
        assert_eq!(scan.records.len(), 1);
        assert_eq!(scan.stopped, Some(LogFormatError::Truncated));
    }

    #[test]
    fn stops_at_a_sequence_gap() {
        let scan = LogScan::segment(&segment(&[10, 12])).unwrap();
        assert_eq!(scan.records.len(), 1);
        assert_eq!(scan.stopped, Some(LogFormatError::SequenceGap));
    }

    #[test]
    fn resumes_over_appended_bytes() {
        let short = segment(&[10]);
        let long = segment(&[10, 11, 12]);
        let mut scan = LogScan::segment(&short).unwrap();
        scan.resume(&long);
        assert_eq!(scan.records.len(), 3);
        assert_eq!(scan.valid_length, long.len() as u64);
    }

    #[test]
    fn every_prefix_scans_without_panicking() {
        let bytes = segment(&[10, 11, 12]);
        for end in LogSegmentHeader::ENCODED_BYTES..bytes.len() {
            let scan = LogScan::segment(&bytes[..end]).unwrap();
            assert!(scan.valid_length <= end as u64);
        }
    }
}
