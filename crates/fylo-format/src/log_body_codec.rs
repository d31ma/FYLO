use std::borrow::Cow;
use std::fmt::Debug;
use std::ops::RangeInclusive;

use crate::{LogFormatError, LogRecord};

/// How a record's body is stored between its frame's length and CRC-32C
/// and the next frame.
///
/// A segment file stores the body as is ([`PlainBody`]). A `.fxdb` file
/// stores a codec header and the compressed body (ADR 0016 section 4),
/// which the storage layer implements. Either way the frame's length and
/// checksum cover the stored bytes, so a scan finds damage without
/// decompressing, and a verifier needs no key.
pub trait LogBodyCodec: Debug + Send + Sync {
    /// The bytes to store for an encoded body.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` when the body cannot be stored, or `InvalidField`
    /// when its codec fails.
    fn store(&self, body: Vec<u8>) -> Result<Vec<u8>, LogFormatError>;

    /// The encoded body that stored bytes hold, at most
    /// [`LogRecord::MAX_BODY_BYTES`]. The frame's checksum already matched.
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedVersion` for a codec this build cannot read, so
    /// that no scan mistakes a newer writer's record for a torn tail, and
    /// `InvalidField` or `Oversized` for stored bytes that do not decode.
    fn load<'a>(&self, stored: &'a [u8]) -> Result<Cow<'a, [u8]>, LogFormatError>;

    /// The lengths a frame's stored body may have.
    fn stored_bytes(&self) -> RangeInclusive<usize>;

    /// The body a frame that does not decode may still hold, best effort and
    /// unchecked, for naming what a damaged record held. `frame` starts at
    /// the frame's length field.
    fn unchecked_body<'a>(&self, frame: &'a [u8]) -> Option<Cow<'a, [u8]>>;
}

/// A body stored as is: the frames of segment and log files.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlainBody;

impl LogBodyCodec for PlainBody {
    fn store(&self, body: Vec<u8>) -> Result<Vec<u8>, LogFormatError> {
        Ok(body)
    }

    fn load<'a>(&self, stored: &'a [u8]) -> Result<Cow<'a, [u8]>, LogFormatError> {
        Ok(Cow::Borrowed(stored))
    }

    fn stored_bytes(&self) -> RangeInclusive<usize> {
        LogRecord::MIN_BODY_BYTES..=LogRecord::MAX_BODY_BYTES
    }

    /// Whatever follows the frame's checksum, so a damaged length still
    /// shows the manifest.
    fn unchecked_body<'a>(&self, frame: &'a [u8]) -> Option<Cow<'a, [u8]>> {
        frame.get(LogRecord::FRAME_BYTES..).map(Cow::Borrowed)
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::ops::RangeInclusive;

    use super::{LogBodyCodec, PlainBody};
    use crate::{
        LogFormatError, LogRecord, LogRecordKind, LogSalvage, LogScan, LogSegmentHeader,
        LogSegmentKind,
    };

    /// A codec byte, then the body reversed: enough to tell stored bytes
    /// from the body, as a compressor would.
    #[derive(Debug)]
    struct Reversed;

    impl LogBodyCodec for Reversed {
        fn store(&self, mut body: Vec<u8>) -> Result<Vec<u8>, LogFormatError> {
            body.reverse();
            body.insert(0, 1);
            Ok(body)
        }

        fn load<'a>(&self, stored: &'a [u8]) -> Result<Cow<'a, [u8]>, LogFormatError> {
            match stored.split_first() {
                Some((1, body)) => Ok(Cow::Owned(body.iter().rev().copied().collect())),
                Some(_) => Err(LogFormatError::UnsupportedVersion),
                None => Err(LogFormatError::InvalidField),
            }
        }

        fn stored_bytes(&self) -> RangeInclusive<usize> {
            1..=LogRecord::MAX_BODY_BYTES + 1
        }

        fn unchecked_body<'a>(&self, frame: &'a [u8]) -> Option<Cow<'a, [u8]>> {
            self.load(frame.get(LogRecord::FRAME_BYTES..)?).ok()
        }
    }

    fn record(lsn: u64) -> LogRecord {
        LogRecord {
            lsn,
            kind: LogRecordKind::Commit,
            generation: 2,
            manifest: br#"{"id":"x"}"#.to_vec(),
            blobs: b"body".to_vec(),
        }
    }

    fn segment(lsns: &[u64], codec: &dyn LogBodyCodec) -> (Vec<u8>, Vec<usize>) {
        let mut bytes = LogSegmentHeader {
            kind: LogSegmentKind::Documents,
            sequence: 1,
            first_lsn: 1,
        }
        .encode()
        .to_vec();
        let mut starts = Vec::new();
        for lsn in lsns {
            starts.push(bytes.len());
            bytes.extend(
                LogSegmentKind::Documents
                    .frame_with(&record(*lsn), codec)
                    .unwrap(),
            );
        }
        (bytes, starts)
    }

    #[test]
    fn a_plain_body_frames_as_before() {
        assert_eq!(
            record(1).encode_with(&PlainBody).unwrap(),
            record(1).encode().unwrap()
        );
    }

    #[test]
    fn a_stored_body_round_trips_and_its_checksum_covers_the_stored_bytes() {
        let framed = LogSegmentKind::Documents
            .frame_with(&record(4), &Reversed)
            .unwrap();
        assert_eq!(
            LogSegmentKind::Documents.unframe_with(&framed, &Reversed),
            Ok((record(4), framed.len()))
        );
        // Read as a plain body, it is not the record.
        assert!(LogSegmentKind::Documents.unframe(&framed).is_err());
        let mut damaged = framed.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 1;
        assert_eq!(
            LogSegmentKind::Documents.unframe_with(&damaged, &Reversed),
            Err(LogFormatError::ChecksumMismatch)
        );
    }

    #[test]
    fn a_scan_stops_at_a_codec_it_cannot_read_and_says_so() {
        let (mut bytes, starts) = segment(&[1, 2, 3], &Reversed);
        let header = starts[1] + LogSegmentKind::MARKER_BYTES + LogRecord::FRAME_BYTES;
        bytes[header] = 9;
        let stored = &bytes[header..starts[2]];
        let checksum = crate::Crc32c::checksum(stored).to_le_bytes();
        bytes[header - 4..header].copy_from_slice(&checksum);
        let scan = LogScan::segment_with(&bytes, &Reversed).unwrap();
        assert_eq!(scan.records.len(), 1);
        assert_eq!(scan.valid_length, starts[1] as u64);
        assert_eq!(scan.stopped, Some(LogFormatError::UnsupportedVersion));
        let salvage = LogSalvage::segment_with(&bytes, &Reversed).unwrap();
        assert_eq!(salvage.records.len(), 2);
        assert_eq!(salvage.damaged, vec![(starts[1] as u64, starts[2] as u64)]);
    }

    #[test]
    fn a_damaged_stored_frame_still_shows_its_manifest_when_it_loads() {
        let (mut bytes, starts) = segment(&[1], &Reversed);
        let frame = starts[0] + LogSegmentKind::MARKER_BYTES;
        bytes[frame + 4] ^= 0xFF;
        assert!(
            LogScan::segment_with(&bytes, &Reversed)
                .unwrap()
                .stopped
                .is_some()
        );
        assert_eq!(
            LogRecord::unchecked_manifest_with(&bytes[frame..], &Reversed).as_deref(),
            Some(&br#"{"id":"x"}"#[..])
        );
        assert_eq!(
            LogRecord::unchecked_manifest_with(&bytes[frame..], &PlainBody),
            None
        );
    }
}
