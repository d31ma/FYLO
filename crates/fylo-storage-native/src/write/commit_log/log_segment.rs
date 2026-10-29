use std::path::{Path, PathBuf};
use std::sync::Arc;

use fylo_format::{LogRecord, LogRecordKind, LogScan, LogSegmentHeader, LogSegmentKind};
use fylo_vfs as fs;

use super::super::{json_error, sync_directory};
use super::checkpoint_manifest::CheckpointManifest;
use crate::segments::{FileSegment, SegmentHandle, SegmentMedium};
use crate::{Durability, LogPosition, NativeStorageError, NativeStorageErrorCode};

/// The open write-ahead log segment of one collection, or the active
/// document segment of a segment collection (ADR 0015).
///
/// A file collection has one log segment at a time. A checkpoint removes it,
/// and the next commit creates a new one whose first record is a checkpoint.
/// A segment collection's active segment is never removed: it is sealed when
/// it grows large, and the next one opens with a checkpoint record. A log
/// segment is a file at a known path; a document segment lives in its
/// collection's [`SegmentMedium`].
#[derive(Debug)]
pub(crate) struct LogSegment {
    kind: LogSegmentKind,
    handle: Arc<dyn SegmentHandle>,
    log_path: Option<PathBuf>,
    max_bytes: u64,
    nonce: u64,
    length: u64,
    next_lsn: u64,
    records: u64,
    fresh: bool,
}

impl LogSegment {
    const FIRST_LSN: u64 = 1;
    const MAX_LOG_BYTES: u64 = 1024 * 1024 * 1024;

    /// Open the log segment `position` describes, or `None` when the file
    /// is missing or differs from it.
    pub(crate) fn open_matching(
        path: &Path,
        position: &LogPosition,
    ) -> Result<Option<Self>, NativeStorageError> {
        let Some(file) = FileSegment::open(path)? else {
            return Ok(None);
        };
        Self::matching(
            Arc::new(file),
            (Some(path.to_owned()), Self::MAX_LOG_BYTES),
            position,
        )
    }

    /// Open the document segment `position` describes, or `None` when it is
    /// missing or differs from it.
    pub(crate) fn open_segment_matching(
        medium: &dyn SegmentMedium,
        position: &LogPosition,
    ) -> Result<Option<Self>, NativeStorageError> {
        let number = u64::from_str_radix(&position.nonce, 16).unwrap_or(u64::MAX);
        let Some(handle) = medium.write_handle(number)? else {
            return Ok(None);
        };
        Self::matching(handle, (None, medium.max_segment_bytes()), position)
    }

    fn matching(
        handle: Arc<dyn SegmentHandle>,
        (log_path, max_bytes): (Option<PathBuf>, u64),
        position: &LogPosition,
    ) -> Result<Option<Self>, NativeStorageError> {
        let length = handle.length()?;
        let header = Self::read_header(&*handle, length)?;
        let matches = header.is_some_and(|header| Self::hex(header.sequence) == position.nonce)
            && length == position.length;
        if !matches {
            return Ok(None);
        }
        Ok(Some(Self {
            kind: header.map_or(LogSegmentKind::WriteAheadLog, |header| header.kind),
            handle,
            log_path,
            max_bytes,
            nonce: header.map_or(0, |header| header.sequence),
            length,
            next_lsn: position.next_lsn,
            records: position.records,
            fresh: false,
        }))
    }

    /// Open a document segment whose valid length a recovery scan found.
    pub(crate) fn open_scanned(
        medium: &dyn SegmentMedium,
        (sequence, length): (u64, u64),
        next_lsn: u64,
    ) -> Result<Self, NativeStorageError> {
        let handle = medium.write_handle(sequence)?.ok_or_else(|| {
            NativeStorageError::io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("segment {sequence} does not exist"),
            ))
        })?;
        handle.truncate(length)?;
        Ok(Self {
            kind: LogSegmentKind::Documents,
            handle,
            log_path: None,
            max_bytes: medium.max_segment_bytes(),
            nonce: sequence,
            length,
            next_lsn,
            records: 0,
            fresh: false,
        })
    }

    /// Create the segment with its opening checkpoint record. Nothing is
    /// flushed: the first commit's flush covers these bytes too.
    pub(crate) fn create(
        path: &Path,
        checkpoint: &CheckpointManifest,
        generation: u64,
    ) -> Result<Self, NativeStorageError> {
        let directory = path.parent().ok_or_else(|| {
            NativeStorageError::new(NativeStorageErrorCode::UnsafePath, "log has no directory")
        })?;
        match fs::create_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(NativeStorageError::io(error)),
        }
        let manifest = serde_json::to_vec(checkpoint).map_err(|error| json_error(&error))?;
        let opening = SegmentOpening {
            kind: LogSegmentKind::WriteAheadLog,
            sequence: Self::random_nonce()?,
            first_lsn: Self::FIRST_LSN,
            generation,
        };
        let bytes = Self::opening_bytes(&opening, manifest)?;
        let file = FileSegment::create(path, &bytes, opening.kind.marked())?;
        Ok(Self::created(
            &opening,
            Arc::new(file),
            (Some(path.to_owned()), Self::MAX_LOG_BYTES),
            bytes.len(),
        ))
    }

    /// Create the collection's next document segment, opening with a
    /// checkpoint record whose manifest is `manifest`, at sequence number
    /// `first_lsn`. The medium numbers it.
    pub(crate) fn create_documents(
        medium: &dyn SegmentMedium,
        first_lsn: u64,
        manifest: &[u8],
        generation: u64,
    ) -> Result<Self, NativeStorageError> {
        let opening = |sequence| SegmentOpening {
            kind: LogSegmentKind::Documents,
            sequence,
            first_lsn,
            generation,
        };
        let length = std::cell::Cell::new(0);
        let (number, handle) = medium.create_segment(&|number| {
            let bytes = Self::opening_bytes(&opening(number), manifest.to_vec())?;
            length.set(bytes.len());
            Ok(bytes)
        })?;
        Ok(Self::created(
            &opening(number),
            handle,
            (None, medium.max_segment_bytes()),
            length.get(),
        ))
    }

    /// A new segment's header and opening checkpoint record.
    fn opening_bytes(
        opening: &SegmentOpening,
        manifest: Vec<u8>,
    ) -> Result<Vec<u8>, NativeStorageError> {
        let header = LogSegmentHeader {
            kind: opening.kind,
            sequence: opening.sequence,
            first_lsn: opening.first_lsn,
        };
        let record = LogRecord {
            lsn: opening.first_lsn,
            kind: LogRecordKind::Checkpoint,
            generation: opening.generation,
            manifest,
            blobs: Vec::new(),
        };
        let mut bytes = header.encode().to_vec();
        bytes.extend(Self::frame(opening.kind, &record)?);
        Ok(bytes)
    }

    fn created(
        opening: &SegmentOpening,
        handle: Arc<dyn SegmentHandle>,
        (log_path, max_bytes): (Option<PathBuf>, u64),
        length: usize,
    ) -> Self {
        Self {
            kind: opening.kind,
            handle,
            log_path,
            max_bytes,
            nonce: opening.sequence,
            length: length as u64,
            next_lsn: opening.first_lsn + 1,
            records: 0,
            fresh: true,
        }
    }

    /// Append encoded records. A failed append is cut back off, so nothing
    /// unacknowledged stays in the log.
    pub(crate) fn append(&mut self, bytes: &[u8], records: u64) -> Result<(), NativeStorageError> {
        crate::write::failpoint("before-log-append")?;
        let outcome = self
            .write_at_end(bytes)
            .and_then(|()| crate::write::failpoint("after-log-append"));
        if let Err(error) = outcome {
            self.truncate(self.length)?;
            return Err(error);
        }
        self.length += bytes.len() as u64;
        self.next_lsn += records;
        self.records += records;
        if self.length > self.max_bytes {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "write-ahead log segment exceeds its limit",
            ));
        }
        Ok(())
    }

    /// Write at the segment's known end, so a failed append can be cut back
    /// off.
    fn write_at_end(&self, bytes: &[u8]) -> Result<(), NativeStorageError> {
        self.handle.write_at(bytes, self.length)
    }

    /// Cut the segment back to `length` bytes.
    pub(crate) fn truncate(&mut self, length: u64) -> Result<(), NativeStorageError> {
        self.handle.truncate(length)?;
        self.length = length;
        Ok(())
    }

    /// Cut the segment back to its last durable point after a failed flush.
    pub(crate) fn rewind(
        &mut self,
        (length, next_lsn, records): (u64, u64, u64),
    ) -> Result<(), NativeStorageError> {
        self.truncate(length)?;
        self.next_lsn = next_lsn;
        self.records = records;
        Ok(())
    }

    /// A handle a flusher can use without holding the queue's state.
    pub(crate) fn flush_handle(&self) -> LogFlush {
        LogFlush {
            handle: Arc::clone(&self.handle),
            created: self.fresh,
            log_parent: self
                .log_path
                .as_deref()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .map(Path::to_owned),
        }
    }

    /// The new directory entries are durable once a flush covered them.
    pub(crate) fn mark_flushed(&mut self) {
        self.fresh = false;
    }

    /// The file of a write-ahead log segment; `None` for a document segment.
    pub(crate) fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    pub(crate) fn next_lsn(&self) -> u64 {
        self.next_lsn
    }

    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    pub(crate) fn position(&self) -> LogPosition {
        LogPosition {
            nonce: Self::hex(self.nonce),
            length: self.length,
            next_lsn: self.next_lsn,
            records: self.records,
        }
    }

    /// Encode one record as this segment's frame, refusing a body the
    /// format cannot hold.
    pub(crate) fn encode(&self, record: &LogRecord) -> Result<Vec<u8>, NativeStorageError> {
        Self::frame(self.kind, record)
    }

    /// Encode one record as a frame of `kind`.
    pub(crate) fn frame(
        kind: LogSegmentKind,
        record: &LogRecord,
    ) -> Result<Vec<u8>, NativeStorageError> {
        kind.frame(record).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                format!("write-ahead log record cannot be encoded: {error}"),
            )
        })
    }

    /// The segment's identity: its number, for a document segment.
    pub(crate) fn sequence(&self) -> u64 {
        self.nonce
    }

    /// Records appended since the segment was opened or last checkpointed.
    pub(crate) fn records(&self) -> u64 {
        self.records
    }

    /// Start counting records again after a checkpoint.
    pub(crate) fn reset_records(&mut self) {
        self.records = 0;
    }

    /// Flush the segment through the drive cache (`F_FULLFSYNC` on Apple),
    /// with its name when the segment is new.
    pub(crate) fn full_flush(&mut self) -> Result<(), NativeStorageError> {
        self.handle.sync(Durability::PowerLoss)?;
        if self.fresh {
            self.handle.sync_created()?;
            self.fresh = false;
        }
        Ok(())
    }

    /// Read and scan a whole log segment, or `None` when it does not exist
    /// or its header never became valid.
    pub(crate) fn scan(path: &Path) -> Result<Option<LogScan>, NativeStorageError> {
        match fs::read(path) {
            Ok(bytes) => Ok(LogScan::segment(&bytes).ok()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    /// Read and scan a whole document segment, or `None` when it does not
    /// exist or its header never became valid.
    pub(crate) fn scan_segment(
        medium: &dyn SegmentMedium,
        number: u64,
    ) -> Result<Option<LogScan>, NativeStorageError> {
        Ok(medium
            .read_segment(number)?
            .and_then(|bytes| LogScan::segment(&bytes).ok()))
    }

    /// The header of a segment `length` bytes long, or `None` when it is
    /// too short to hold one or its header is not valid.
    fn read_header(
        handle: &dyn SegmentHandle,
        length: u64,
    ) -> Result<Option<LogSegmentHeader>, NativeStorageError> {
        let mut bytes = [0_u8; LogSegmentHeader::ENCODED_BYTES];
        if length < bytes.len() as u64 {
            return Ok(None);
        }
        handle.read_at(&mut bytes, 0)?;
        Ok(LogSegmentHeader::decode(&bytes).ok())
    }

    fn random_nonce() -> Result<u64, NativeStorageError> {
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::Io,
                format!("cannot draw a log segment identity: {error}"),
            )
        })?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn hex(value: u64) -> String {
        format!("{value:016x}")
    }
}

/// How a new segment starts.
struct SegmentOpening {
    kind: LogSegmentKind,
    sequence: u64,
    first_lsn: u64,
    generation: u64,
}

/// What a flush needs, held outside the commit queue's lock.
#[derive(Debug)]
pub(crate) struct LogFlush {
    handle: Arc<dyn SegmentHandle>,
    created: bool,
    log_parent: Option<PathBuf>,
}

impl LogFlush {
    /// Flush the segment and, when it is new, its name. A new log segment
    /// may sit in a new directory, so that directory's parent is flushed
    /// too.
    pub(crate) fn run(&self, durability: Durability) -> Result<(), NativeStorageError> {
        crate::write::failpoint("before-log-sync")?;
        self.handle.sync(durability)?;
        if self.created {
            self.handle.sync_created()?;
            if let Some(parent) = &self.log_parent {
                sync_directory(parent)?;
            }
        }
        crate::write::failpoint("after-log-sync")
    }
}
