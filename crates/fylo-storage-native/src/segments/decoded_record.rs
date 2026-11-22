use fylo_format::LogRecord;

use super::{SegmentCommitManifest, SegmentDocument, SegmentLocation};
use crate::NativeStorageError;

/// A commit record with its manifest decoded once, so every document read
/// out of it shares that one decode.
#[derive(Debug)]
pub(crate) struct DecodedRecord {
    record: LogRecord,
    manifest: SegmentCommitManifest,
}

impl DecodedRecord {
    pub(crate) fn new(record: LogRecord) -> Result<Self, NativeStorageError> {
        let manifest = SegmentCommitManifest::decode(&record.manifest)?;
        Ok(Self { record, manifest })
    }

    /// The record as it was read.
    pub(crate) fn record(&self) -> &LogRecord {
        &self.record
    }

    /// The document `location` names inside this record.
    pub(crate) fn document(
        &self,
        location: &SegmentLocation,
    ) -> Result<SegmentDocument, NativeStorageError> {
        SegmentDocument::from_manifest(location, &self.record, &self.manifest)
    }
}
