use crate::{LogBodyCodec, LogFormatError, LogRecord, PlainBody};

/// Which of the two segment families a segment belongs to.
///
/// A write-ahead log segment (ADR 0013) is replayed into per-document files
/// and removed at a checkpoint. A document segment (ADR 0015) is where a
/// segment collection keeps its documents: its records carry a sync marker in
/// front of every frame, so a salvage scan can step past a damaged record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogSegmentKind {
    /// A collection write-ahead log segment.
    WriteAheadLog,
    /// A document segment written by commits, with consecutive sequence
    /// numbers.
    Documents,
    /// A document segment written by compaction. Each record keeps the
    /// sequence number of the commit it was copied from, so the numbers are
    /// unique but not consecutive.
    CompactedDocuments,
}

impl LogSegmentKind {
    /// Bytes of the sync marker in front of a document segment frame.
    pub const MARKER_BYTES: usize = 4;
    const MARKER: [u8; Self::MARKER_BYTES] = [0xF5, 0x59, 0x52, 0x01];
    const LOG_MAGIC: [u8; 8] = *b"FYLOLOG\0";
    const DOCUMENT_MAGIC: [u8; 8] = *b"FYLOSEG\0";
    const COMPACTED_FLAG: u16 = 1;

    /// The magic that opens a segment of this kind.
    #[must_use]
    pub const fn magic(self) -> [u8; 8] {
        match self {
            Self::WriteAheadLog => Self::LOG_MAGIC,
            Self::Documents | Self::CompactedDocuments => Self::DOCUMENT_MAGIC,
        }
    }

    /// The header flags of a segment of this kind.
    #[must_use]
    pub const fn flags(self) -> u16 {
        match self {
            Self::CompactedDocuments => Self::COMPACTED_FLAG,
            Self::WriteAheadLog | Self::Documents => 0,
        }
    }

    /// Whether `magic` opens a segment of any kind.
    #[must_use]
    pub fn known_magic(magic: &[u8]) -> bool {
        magic == Self::LOG_MAGIC || magic == Self::DOCUMENT_MAGIC
    }

    /// The kind a header's magic and flags name.
    ///
    /// # Errors
    ///
    /// Returns `BadMagic` for an unknown magic and `InvalidField` for flags
    /// the magic does not allow.
    pub fn from_header(magic: &[u8], flags: u16) -> Result<Self, LogFormatError> {
        let kind = if magic == Self::LOG_MAGIC {
            Self::WriteAheadLog
        } else if magic == Self::DOCUMENT_MAGIC {
            Self::document_kind(flags)?
        } else {
            return Err(LogFormatError::BadMagic);
        };
        if kind.flags() != flags {
            return Err(LogFormatError::InvalidField);
        }
        Ok(kind)
    }

    /// Whether each record's sequence number must follow the previous one.
    #[must_use]
    pub const fn consecutive(self) -> bool {
        !matches!(self, Self::CompactedDocuments)
    }

    /// Whether frames of this kind carry the sync marker.
    #[must_use]
    pub const fn marked(self) -> bool {
        !matches!(self, Self::WriteAheadLog)
    }

    /// Encode one record as a frame of this kind.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` when the body would exceed the record limit.
    pub fn frame(self, record: &LogRecord) -> Result<Vec<u8>, LogFormatError> {
        self.frame_with(record, &PlainBody)
    }

    /// Encode one record as a frame of this kind, its body stored by
    /// `codec`.
    ///
    /// # Errors
    ///
    /// Returns what [`LogRecord::encode_with`] does.
    pub fn frame_with(
        self,
        record: &LogRecord,
        codec: &dyn LogBodyCodec,
    ) -> Result<Vec<u8>, LogFormatError> {
        let encoded = record.encode_with(codec)?;
        if !self.marked() {
            return Ok(encoded);
        }
        let mut framed = Vec::with_capacity(Self::MARKER_BYTES + encoded.len());
        framed.extend_from_slice(&Self::MARKER);
        framed.extend_from_slice(&encoded);
        Ok(framed)
    }

    /// Decode one frame of this kind from the start of `bytes`.
    ///
    /// Returns the record and the number of bytes the frame occupied.
    ///
    /// # Errors
    ///
    /// Returns `InvalidField` when a marked frame does not start with the
    /// marker, or the defect [`LogRecord::decode`] reports.
    pub fn unframe(self, bytes: &[u8]) -> Result<(LogRecord, usize), LogFormatError> {
        self.unframe_with(bytes, &PlainBody)
    }

    /// Decode one frame of this kind whose body `codec` stored.
    ///
    /// # Errors
    ///
    /// Returns what [`Self::unframe`] does, or the codec's failure.
    pub fn unframe_with(
        self,
        bytes: &[u8],
        codec: &dyn LogBodyCodec,
    ) -> Result<(LogRecord, usize), LogFormatError> {
        if !self.marked() {
            return LogRecord::decode_with(bytes, codec);
        }
        let marker = bytes
            .get(..Self::MARKER_BYTES)
            .ok_or(LogFormatError::Truncated)?;
        if marker != Self::MARKER {
            return Err(LogFormatError::InvalidField);
        }
        let (record, length) = LogRecord::decode_with(&bytes[Self::MARKER_BYTES..], codec)?;
        Ok((record, Self::MARKER_BYTES + length))
    }

    /// The stored body of the frame of this kind at the start of `bytes`,
    /// checked but not loaded, and the bytes the frame occupies
    /// ([`LogRecord::stored_body`]).
    ///
    /// # Errors
    ///
    /// Returns `InvalidField` when a marked frame does not start with the
    /// marker, or what [`LogRecord::stored_body`] returns.
    pub fn stored_frame<'a>(
        self,
        bytes: &'a [u8],
        codec: &dyn LogBodyCodec,
    ) -> Result<(&'a [u8], usize), LogFormatError> {
        if !self.marked() {
            return LogRecord::stored_body(bytes, codec);
        }
        let marker = bytes
            .get(..Self::MARKER_BYTES)
            .ok_or(LogFormatError::Truncated)?;
        if marker != Self::MARKER {
            return Err(LogFormatError::InvalidField);
        }
        let (body, length) = LogRecord::stored_body(&bytes[Self::MARKER_BYTES..], codec)?;
        Ok((body, Self::MARKER_BYTES + length))
    }

    /// The offset of the next sync marker at or after `from`, if any.
    #[must_use]
    pub fn next_marker(bytes: &[u8], from: usize) -> Option<usize> {
        let window = bytes.get(from..)?;
        window
            .windows(Self::MARKER_BYTES)
            .position(|candidate| candidate == Self::MARKER)
            .map(|position| from + position)
    }

    fn document_kind(flags: u16) -> Result<Self, LogFormatError> {
        match flags {
            0 => Ok(Self::Documents),
            Self::COMPACTED_FLAG => Ok(Self::CompactedDocuments),
            _ => Err(LogFormatError::InvalidField),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LogSegmentKind;
    use crate::{LogFormatError, LogRecord, LogRecordKind};

    fn record() -> LogRecord {
        LogRecord {
            lsn: 4,
            kind: LogRecordKind::Commit,
            generation: 2,
            manifest: b"{}".to_vec(),
            blobs: b"body".to_vec(),
        }
    }

    #[test]
    fn a_marked_frame_round_trips() {
        let framed = LogSegmentKind::Documents.frame(&record()).unwrap();
        assert_eq!(
            LogSegmentKind::Documents.unframe(&framed),
            Ok((record(), framed.len()))
        );
    }

    #[test]
    fn a_log_frame_has_no_marker() {
        let framed = LogSegmentKind::WriteAheadLog.frame(&record()).unwrap();
        assert_eq!(framed, record().encode().unwrap());
    }

    #[test]
    fn a_frame_without_its_marker_is_refused() {
        let framed = LogSegmentKind::WriteAheadLog.frame(&record()).unwrap();
        assert_eq!(
            LogSegmentKind::Documents.unframe(&framed),
            Err(LogFormatError::InvalidField)
        );
    }

    #[test]
    fn kinds_round_trip_through_their_header_fields() {
        for kind in [
            LogSegmentKind::WriteAheadLog,
            LogSegmentKind::Documents,
            LogSegmentKind::CompactedDocuments,
        ] {
            assert_eq!(
                LogSegmentKind::from_header(&kind.magic(), kind.flags()),
                Ok(kind)
            );
        }
    }

    #[test]
    fn a_log_with_flags_is_refused() {
        assert_eq!(
            LogSegmentKind::from_header(b"FYLOLOG\0", 1),
            Err(LogFormatError::InvalidField)
        );
    }

    #[test]
    fn finds_the_next_marker() {
        let mut bytes = vec![0_u8; 5];
        bytes.extend(LogSegmentKind::Documents.frame(&record()).unwrap());
        assert_eq!(LogSegmentKind::next_marker(&bytes, 0), Some(5));
        assert_eq!(LogSegmentKind::next_marker(&bytes, 6), None);
    }
}
