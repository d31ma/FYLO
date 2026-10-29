use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use fylo_format::{LogRecord, LogSegmentKind};
use fylo_vfs::File;

use super::{PositionedRead, SegmentLocation, SegmentPaths};
use crate::{NativeStorageError, NativeStorageErrorCode};

/// Reads records out of a collection's segments, keeping each segment open
/// once it has been read.
#[derive(Debug)]
pub(crate) struct SegmentReader {
    paths: SegmentPaths,
    files: Mutex<BTreeMap<u64, Arc<File>>>,
}

impl SegmentReader {
    pub(crate) fn new(paths: SegmentPaths) -> Self {
        Self {
            paths,
            files: Mutex::new(BTreeMap::new()),
        }
    }

    /// `length` bytes of segment `segment` starting at `offset`.
    pub(crate) fn span(
        &self,
        segment: u64,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, NativeStorageError> {
        let file = self.file(segment)?;
        let mut bytes = vec![0_u8; length];
        PositionedRead::exact(&file, &mut bytes, offset).map_err(Self::vanished)?;
        Ok(bytes)
    }

    /// The record a location points at.
    pub(crate) fn record(
        &self,
        location: &SegmentLocation,
    ) -> Result<LogRecord, NativeStorageError> {
        let bytes = self.span(location.segment, location.offset, location.length as usize)?;
        Self::decode(&bytes)
    }

    /// Decode one whole frame, checking its marker, length and checksum.
    pub(crate) fn decode(frame: &[u8]) -> Result<LogRecord, NativeStorageError> {
        match LogSegmentKind::Documents.unframe(frame) {
            Ok((record, length)) if length == frame.len() => Ok(record),
            Ok(_) => Err(Self::corrupt(
                "segment record length does not match its location",
            )),
            Err(error) => Err(Self::corrupt(&format!(
                "segment record is damaged: {error}"
            ))),
        }
    }

    fn file(&self, segment: u64) -> Result<Arc<File>, NativeStorageError> {
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(file) = files.get(&segment) {
            return Ok(Arc::clone(file));
        }
        let file = Arc::new(
            File::open(self.paths.segment(segment))
                .map_err(NativeStorageError::io)
                .map_err(Self::vanished)?,
        );
        files.insert(segment, Arc::clone(&file));
        Ok(file)
    }

    /// A segment that compaction removed, or that ends before a location,
    /// means the generation moved under the reader, which readers retry.
    fn vanished(error: NativeStorageError) -> NativeStorageError {
        let kind = error.source.as_ref().map(std::io::Error::kind);
        let moved = matches!(
            kind,
            Some(std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof)
        ) || (cfg!(windows) && kind == Some(std::io::ErrorKind::PermissionDenied));
        if !moved {
            return error;
        }
        NativeStorageError::new(
            NativeStorageErrorCode::ConcurrentWrite,
            "a segment changed while it was read",
        )
    }

    fn corrupt(message: &str) -> NativeStorageError {
        NativeStorageError::new(NativeStorageErrorCode::CorruptDocument, message)
    }
}
