use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use fylo_format::{LogBodyCodec, LogFormatError, LogRecord, LogSegmentKind};

use super::{SegmentHandle, SegmentLocation, SegmentMedium};
use crate::{NativeStorageError, NativeStorageErrorCode};

/// Reads records out of a collection's segments, keeping each segment open
/// once it has been read.
#[derive(Debug)]
pub(crate) struct SegmentReader {
    medium: Arc<dyn SegmentMedium>,
    handles: Mutex<BTreeMap<u64, Arc<dyn SegmentHandle>>>,
}

impl SegmentReader {
    pub(crate) fn new(medium: Arc<dyn SegmentMedium>) -> Self {
        Self {
            medium,
            handles: Mutex::new(BTreeMap::new()),
        }
    }

    /// `length` bytes of segment `segment` starting at `offset`.
    pub(crate) fn span(
        &self,
        segment: u64,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, NativeStorageError> {
        let handle = self.handle(segment)?;
        let mut bytes = vec![0_u8; length];
        handle.read_at(&mut bytes, offset)?;
        Ok(bytes)
    }

    /// The record a location points at.
    pub(crate) fn record(
        &self,
        location: &SegmentLocation,
    ) -> Result<LogRecord, NativeStorageError> {
        let bytes = self.span(location.segment, location.offset, location.length as usize)?;
        Self::decode(&bytes, &*self.medium.frame_bodies())
    }

    /// The medium the reader reads.
    pub(crate) fn medium(&self) -> &dyn SegmentMedium {
        &*self.medium
    }

    /// Decode one whole frame whose body `bodies` stored, checking its
    /// marker, length and checksum.
    pub(crate) fn decode(
        frame: &[u8],
        bodies: &dyn LogBodyCodec,
    ) -> Result<LogRecord, NativeStorageError> {
        match LogSegmentKind::Documents.unframe_with(frame, bodies) {
            Ok((record, length)) if length == frame.len() => Ok(record),
            Ok(_) => Err(Self::corrupt(
                "segment record length does not match its location",
            )),
            Err(error) => Err(Self::frame_error(error, "segment record")),
        }
    }

    /// A frame that does not decode, as a storage error. One this build
    /// cannot read, such as a record compressed with a codec it does not
    /// know, is refused as a newer format (`EFXDB_VERSION`), never taken
    /// for damage or a torn tail; anything else is damage.
    pub(crate) fn frame_error(error: LogFormatError, what: &str) -> NativeStorageError {
        if error == LogFormatError::UnsupportedVersion {
            NativeStorageError::new(
                NativeStorageErrorCode::Unsupported,
                format!(
                    "{what} uses a record codec or format this build cannot read \
                     (EFXDB_VERSION)"
                ),
            )
        } else {
            Self::corrupt(&format!("{what} is damaged: {error}"))
        }
    }

    fn handle(&self, segment: u64) -> Result<Arc<dyn SegmentHandle>, NativeStorageError> {
        let mut handles = self.handles.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = handles.get(&segment) {
            return Ok(Arc::clone(handle));
        }
        let handle = self.medium.read_handle(segment)?;
        handles.insert(segment, Arc::clone(&handle));
        Ok(handle)
    }

    fn corrupt(message: &str) -> NativeStorageError {
        NativeStorageError::new(NativeStorageErrorCode::CorruptDocument, message)
    }
}
