use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fylo_format::LogRecord;

use super::{LocationState, SegmentCommitManifest, SegmentEntry, SegmentLocation};
use crate::{AccessDescriptor, NativeStorageError, NativeStorageErrorCode};

/// One document as its newest segment entry holds it.
#[derive(Clone, Debug)]
pub(crate) struct SegmentDocument {
    pub(crate) location: SegmentLocation,
    pub(crate) entry: SegmentEntry,
    pub(crate) body: Vec<u8>,
}

impl SegmentDocument {
    /// The document `location` names inside `record`.
    ///
    /// The record must be the one the location names, by LSN as well as by
    /// entry: an older record of the same document put in its place, as a
    /// replayed frame of an encrypted file would be, is refused rather than
    /// read as the newest. A row that names no LSN, which no writer leaves
    /// for a live document, is checked by entry alone.
    pub(crate) fn from_record(
        location: &SegmentLocation,
        record: &LogRecord,
    ) -> Result<Self, NativeStorageError> {
        if location.lsn != 0 && record.lsn != location.lsn {
            return Err(Self::corrupt(
                "segment record is not the one its location names",
            ));
        }
        let manifest = SegmentCommitManifest::decode(&record.manifest)?;
        let entry = manifest
            .entries
            .into_iter()
            .nth(usize::from(location.entry))
            .filter(|entry| entry.id == location.id && entry.state == location.state)
            .ok_or_else(|| Self::corrupt("segment entry does not match its location"))?;
        let body = Self::body_of(&entry, &record.blobs)?;
        Ok(Self {
            location: location.clone(),
            entry,
            body,
        })
    }

    /// The entry's attributes, decoded to their raw values.
    pub(crate) fn attributes(&self) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        Self::decode_attributes(&self.entry.attributes)
    }

    /// The document as the collection API returns it.
    pub(crate) fn into_stored(
        self,
        segment: std::path::PathBuf,
    ) -> Result<crate::StoredBytes, NativeStorageError> {
        Ok(crate::StoredBytes {
            access: self.access()?,
            modified_millis: self.entry.modified_ms,
            bytes: self.body,
            path: segment,
        })
    }

    pub(crate) fn access(&self) -> Result<Option<AccessDescriptor>, NativeStorageError> {
        crate::decode_access_descriptor(&self.attributes()?)
    }

    pub(crate) fn decode_attributes(
        attributes: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, Vec<u8>>, NativeStorageError> {
        attributes
            .iter()
            .map(|(name, value)| {
                BASE64
                    .decode(value)
                    .map(|bytes| (name.clone(), bytes))
                    .map_err(|_| Self::corrupt("segment entry attribute is not base64"))
            })
            .collect()
    }

    fn body_of(entry: &SegmentEntry, blobs: &[u8]) -> Result<Vec<u8>, NativeStorageError> {
        if entry.state == LocationState::Absent {
            return Ok(Vec::new());
        }
        let body = entry
            .body
            .as_ref()
            .ok_or_else(|| Self::corrupt("segment entry has no body"))?;
        let start = usize::try_from(body.offset).ok();
        let end = start
            .zip(usize::try_from(body.length).ok())
            .and_then(|(start, length)| start.checked_add(length));
        start
            .zip(end)
            .and_then(|(start, end)| blobs.get(start..end))
            .map(<[u8]>::to_vec)
            .ok_or_else(|| Self::corrupt("segment entry body lies outside its record"))
    }

    fn corrupt(message: &str) -> NativeStorageError {
        NativeStorageError::new(NativeStorageErrorCode::CorruptDocument, message)
    }
}
