use std::borrow::Cow;

use crate::{Crc32c, LogBodyCodec, LogFormatError, LogRecordKind, PlainBody};

/// One framed record in a collection log segment.
///
/// The frame is a little-endian body length, the body's CRC-32C, then the
/// body: sequence number, kind, generation, manifest length, the manifest
/// (canonical JSON chosen by the storage layer) and the concatenated blobs the
/// manifest refers to. A [`LogBodyCodec`] may store the body transformed, as
/// a `.fxdb` file compresses it; the length and checksum are then those of the
/// stored bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogRecord {
    /// Log sequence number.
    pub lsn: u64,
    /// What the record does.
    pub kind: LogRecordKind,
    /// Collection generation the record publishes.
    pub generation: u64,
    /// Versioned JSON manifest.
    pub manifest: Vec<u8>,
    /// Blob bytes the manifest addresses by offset and length.
    pub blobs: Vec<u8>,
}

impl LogRecord {
    /// Largest body a record may carry.
    pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
    /// Bytes of the length and checksum that precede the body.
    pub const FRAME_BYTES: usize = 8;
    /// Smallest body: a record with an empty manifest and no blobs.
    pub const MIN_BODY_BYTES: usize = Self::FIXED_BODY_BYTES;
    const FIXED_BODY_BYTES: usize = 21;

    /// Encode the framed record.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` when the body would exceed [`Self::MAX_BODY_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, LogFormatError> {
        self.encode_with(&PlainBody)
    }

    /// Encode the framed record with its body stored by `codec`.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` when the body would exceed [`Self::MAX_BODY_BYTES`]
    /// or its stored bytes the codec's bounds, or the codec's failure.
    pub fn encode_with(&self, codec: &dyn LogBodyCodec) -> Result<Vec<u8>, LogFormatError> {
        let body_length = Self::FIXED_BODY_BYTES
            .checked_add(self.manifest.len())
            .and_then(|length| length.checked_add(self.blobs.len()))
            .filter(|length| *length <= Self::MAX_BODY_BYTES)
            .ok_or(LogFormatError::Oversized)?;
        let manifest_length =
            u32::try_from(self.manifest.len()).map_err(|_| LogFormatError::Oversized)?;
        let mut body = Vec::with_capacity(body_length);
        body.extend_from_slice(&self.lsn.to_le_bytes());
        body.push(self.kind.code());
        body.extend_from_slice(&self.generation.to_le_bytes());
        body.extend_from_slice(&manifest_length.to_le_bytes());
        body.extend_from_slice(&self.manifest);
        body.extend_from_slice(&self.blobs);
        let body = codec.store(body)?;
        if !codec.stored_bytes().contains(&body.len()) {
            return Err(LogFormatError::Oversized);
        }
        let mut framed = Vec::with_capacity(Self::FRAME_BYTES + body.len());
        let length = u32::try_from(body.len()).map_err(|_| LogFormatError::Oversized)?;
        framed.extend_from_slice(&length.to_le_bytes());
        framed.extend_from_slice(&Crc32c::checksum(&body).to_le_bytes());
        framed.extend_from_slice(&body);
        Ok(framed)
    }

    /// Decode one framed record from the start of `bytes`.
    ///
    /// Returns the record and the number of bytes it occupied.
    ///
    /// # Errors
    ///
    /// Returns the defect that stops a scan here: a short frame, an oversized
    /// length, a checksum mismatch, an unknown kind, or a manifest length that
    /// overruns the body.
    pub fn decode(bytes: &[u8]) -> Result<(Self, usize), LogFormatError> {
        Self::decode_with(bytes, &PlainBody)
    }

    /// Decode one framed record whose body `codec` stored, from the start
    /// of `bytes`.
    ///
    /// Returns the record and the number of bytes it occupied.
    ///
    /// # Errors
    ///
    /// Returns what [`Self::decode`] does, checked against the stored bytes,
    /// or the codec's failure to load them: `UnsupportedVersion` for a
    /// codec this build cannot read.
    pub fn decode_with(
        bytes: &[u8],
        codec: &dyn LogBodyCodec,
    ) -> Result<(Self, usize), LogFormatError> {
        let frame = bytes
            .get(..Self::FRAME_BYTES)
            .ok_or(LogFormatError::Truncated)?;
        let length = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        if !codec.stored_bytes().contains(&length) {
            return Err(LogFormatError::Oversized);
        }
        let end = Self::FRAME_BYTES + length;
        let body = bytes
            .get(Self::FRAME_BYTES..end)
            .ok_or(LogFormatError::Truncated)?;
        let checksum = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
        if Crc32c::checksum(body) != checksum {
            return Err(LogFormatError::ChecksumMismatch);
        }
        let body = codec.load(body)?;
        if body.len() < Self::FIXED_BODY_BYTES {
            return Err(LogFormatError::InvalidField);
        }
        if body.len() > Self::MAX_BODY_BYTES {
            return Err(LogFormatError::Oversized);
        }
        Ok((Self::decode_body(&body)?, end))
    }

    /// The manifest bytes of a frame that does not decode, read as far as
    /// its length fields allow and without the checksum, or `None` when
    /// they reach past `bytes`.
    ///
    /// Nothing here is verified: it is for naming, best effort, what a
    /// damaged record held, and the caller checks whatever it reads.
    #[must_use]
    pub fn unchecked_manifest(bytes: &[u8]) -> Option<&[u8]> {
        let body = bytes.get(Self::FRAME_BYTES..)?;
        Self::manifest_of(body)
    }

    /// [`Self::unchecked_manifest`] of a frame whose body `codec` stored,
    /// as far as the codec can still load it.
    #[must_use]
    pub fn unchecked_manifest_with<'a>(
        bytes: &'a [u8],
        codec: &dyn LogBodyCodec,
    ) -> Option<Cow<'a, [u8]>> {
        match codec.unchecked_body(bytes)? {
            Cow::Borrowed(body) => Self::manifest_of(body).map(Cow::Borrowed),
            Cow::Owned(body) => {
                Self::manifest_of(&body).map(|manifest| Cow::Owned(manifest.to_vec()))
            }
        }
    }

    fn manifest_of(body: &[u8]) -> Option<&[u8]> {
        let length = body.get(17..Self::FIXED_BODY_BYTES)?;
        let length = u32::from_le_bytes([length[0], length[1], length[2], length[3]]) as usize;
        body.get(Self::FIXED_BODY_BYTES..Self::FIXED_BODY_BYTES.checked_add(length)?)
    }

    fn decode_body(body: &[u8]) -> Result<Self, LogFormatError> {
        let manifest_length = u32::from_le_bytes([body[17], body[18], body[19], body[20]]) as usize;
        let manifest_end = Self::FIXED_BODY_BYTES
            .checked_add(manifest_length)
            .filter(|end| *end <= body.len())
            .ok_or(LogFormatError::InvalidField)?;
        Ok(Self {
            lsn: u64::from_le_bytes(eight(&body[0..8])),
            kind: LogRecordKind::from_code(body[8])?,
            generation: u64::from_le_bytes(eight(&body[9..17])),
            manifest: body[Self::FIXED_BODY_BYTES..manifest_end].to_vec(),
            blobs: body[manifest_end..].to_vec(),
        })
    }
}

fn eight(bytes: &[u8]) -> [u8; 8] {
    let mut value = [0_u8; 8];
    value.copy_from_slice(bytes);
    value
}

#[cfg(test)]
mod tests {
    use super::{LogFormatError, LogRecord, LogRecordKind};

    fn record() -> LogRecord {
        LogRecord {
            lsn: 9,
            kind: LogRecordKind::Commit,
            generation: 12,
            manifest: br#"{"format":"x"}"#.to_vec(),
            blobs: b"payload".to_vec(),
        }
    }

    #[test]
    fn round_trips_and_reports_its_length() {
        let encoded = record().encode().unwrap();
        assert_eq!(LogRecord::decode(&encoded), Ok((record(), encoded.len())));
    }

    #[test]
    fn every_truncation_is_detected() {
        let encoded = record().encode().unwrap();
        for end in 0..encoded.len() {
            assert!(LogRecord::decode(&encoded[..end]).is_err(), "prefix {end}");
        }
    }

    #[test]
    fn every_flipped_bit_is_detected() {
        let encoded = record().encode().unwrap();
        for index in 0..encoded.len() {
            for bit in 0..8 {
                let mut damaged = encoded.clone();
                damaged[index] ^= 1 << bit;
                assert_ne!(
                    LogRecord::decode(&damaged).map(|(decoded, _)| decoded),
                    Ok(record()),
                    "byte {index} bit {bit}"
                );
            }
        }
    }

    #[test]
    fn a_damaged_frame_still_shows_its_manifest() {
        let mut encoded = record().encode().unwrap();
        let last = encoded.len() - 1;
        encoded[last] ^= 0xFF;
        assert_eq!(
            LogRecord::decode(&encoded).map(|(decoded, _)| decoded),
            Err(LogFormatError::ChecksumMismatch)
        );
        assert_eq!(
            LogRecord::unchecked_manifest(&encoded),
            Some(&br#"{"format":"x"}"#[..])
        );
        assert_eq!(LogRecord::unchecked_manifest(&encoded[..30]), None);
        assert_eq!(LogRecord::unchecked_manifest(&[]), None);
    }

    #[test]
    fn refuses_an_oversized_length() {
        let mut encoded = record().encode().unwrap();
        encoded[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            LogRecord::decode(&encoded).map(|(decoded, _)| decoded),
            Err(LogFormatError::Oversized)
        );
    }
}
