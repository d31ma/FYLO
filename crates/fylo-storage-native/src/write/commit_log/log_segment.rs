use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fylo_format::{LogRecord, LogRecordKind, LogScan, LogSegmentHeader, LogSegmentKind};
use fylo_vfs::{self as fs, File, OpenOptions};

use super::super::{json_error, sync_directory};
use super::checkpoint_manifest::CheckpointManifest;
use crate::{Durability, LogPosition, NativeStorageError, NativeStorageErrorCode};

/// The open write-ahead log segment of one collection, or the active
/// document segment of a segment collection (ADR 0015).
///
/// A file collection has one log segment at a time. A checkpoint removes it,
/// and the next commit creates a new one whose first record is a checkpoint.
/// A segment collection's active segment is never removed: it is sealed when
/// it grows large, and the next one opens with a checkpoint record.
#[derive(Debug)]
pub(crate) struct LogSegment {
    kind: LogSegmentKind,
    file: Arc<File>,
    path: PathBuf,
    nonce: u64,
    length: u64,
    next_lsn: u64,
    records: u64,
    fresh: bool,
}

impl LogSegment {
    const FIRST_LSN: u64 = 1;
    const MAX_SEGMENT_BYTES: u64 = 1024 * 1024 * 1024;

    /// Open the segment `position` describes, or `None` when the file is
    /// missing or differs from it.
    pub(crate) fn open_matching(
        path: &Path,
        position: &LogPosition,
    ) -> Result<Option<Self>, NativeStorageError> {
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(NativeStorageError::io(error)),
        };
        let length = file.metadata().map_err(NativeStorageError::io)?.len();
        let header = Self::read_header(&file)?;
        let matches = header.is_some_and(|header| Self::hex(header.sequence) == position.nonce)
            && length == position.length;
        if !matches {
            return Ok(None);
        }
        Ok(Some(Self {
            kind: header.map_or(LogSegmentKind::WriteAheadLog, |header| header.kind),
            file: Arc::new(file),
            path: path.to_owned(),
            nonce: header.map_or(0, |header| header.sequence),
            length,
            next_lsn: position.next_lsn,
            records: position.records,
            fresh: false,
        }))
    }

    /// Open a document segment whose valid length a recovery scan found.
    pub(crate) fn open_scanned(
        path: &Path,
        (sequence, length): (u64, u64),
        next_lsn: u64,
    ) -> Result<Self, NativeStorageError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(NativeStorageError::io)?;
        file.set_len(length).map_err(NativeStorageError::io)?;
        Ok(Self {
            kind: LogSegmentKind::Documents,
            file: Arc::new(file),
            path: path.to_owned(),
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
        Self::create_with(path, &opening, manifest)
    }

    /// Create document segment `number`, opening with a checkpoint record
    /// whose manifest is `manifest`, at sequence number `first_lsn`.
    pub(crate) fn create_documents(
        path: &Path,
        (number, first_lsn): (u64, u64),
        manifest: Vec<u8>,
        generation: u64,
    ) -> Result<Self, NativeStorageError> {
        let opening = SegmentOpening {
            kind: LogSegmentKind::Documents,
            sequence: number,
            first_lsn,
            generation,
        };
        Self::create_with(path, &opening, manifest)
    }

    fn create_with(
        path: &Path,
        opening: &SegmentOpening,
        manifest: Vec<u8>,
    ) -> Result<Self, NativeStorageError> {
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
        let mut options = OpenOptions::new();
        let _ = options.read(true).write(true).create_new(true);
        Self::owner_only(&mut options, opening.kind);
        let mut file = options.open(path).map_err(NativeStorageError::io)?;
        file.write_all(&bytes).map_err(NativeStorageError::io)?;
        Ok(Self {
            kind: opening.kind,
            file: Arc::new(file),
            path: path.to_owned(),
            nonce: opening.sequence,
            length: bytes.len() as u64,
            next_lsn: opening.first_lsn + 1,
            records: 0,
            fresh: true,
        })
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
        if self.length > Self::MAX_SEGMENT_BYTES {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "write-ahead log segment exceeds its limit",
            ));
        }
        Ok(())
    }

    /// Write at the segment's known end. The handle is not opened for
    /// appending, because Windows cannot truncate an append-only handle, and a
    /// failed append must be cut back off.
    fn write_at_end(&self, bytes: &[u8]) -> Result<(), NativeStorageError> {
        let mut file = &*self.file;
        file.seek(SeekFrom::Start(self.length))
            .and_then(|_| file.write_all(bytes))
            .map_err(NativeStorageError::io)
    }

    /// Cut the segment back to `length` bytes.
    pub(crate) fn truncate(&mut self, length: u64) -> Result<(), NativeStorageError> {
        self.file.set_len(length).map_err(NativeStorageError::io)?;
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
            file: Arc::clone(&self.file),
            directory: self
                .fresh
                .then(|| self.path.parent().map(Path::to_owned))
                .flatten(),
        }
    }

    /// The new directory entries are durable once a flush covered them.
    pub(crate) fn mark_flushed(&mut self) {
        self.fresh = false;
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
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
    /// with its directory entry when the segment is new.
    pub(crate) fn full_flush(&mut self) -> Result<(), NativeStorageError> {
        self.file.sync_all().map_err(NativeStorageError::io)?;
        if self.fresh {
            if let Some(directory) = self.path.parent() {
                sync_directory(directory)?;
            }
            self.fresh = false;
        }
        Ok(())
    }

    /// Read and scan a whole segment, or `None` when it does not exist or
    /// its header never became valid.
    pub(crate) fn scan(path: &Path) -> Result<Option<LogScan>, NativeStorageError> {
        match fs::read(path) {
            Ok(bytes) => Ok(LogScan::segment(&bytes).ok()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn read_header(file: &File) -> Result<Option<LogSegmentHeader>, NativeStorageError> {
        let mut bytes = [0_u8; LogSegmentHeader::ENCODED_BYTES];
        let mut reader = file;
        match std::io::Read::read_exact(&mut reader, &mut bytes) {
            Ok(()) => Ok(LogSegmentHeader::decode(&bytes).ok()),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    /// A document segment is readable by the root owner only (ADR 0015
    /// section 10). A log segment keeps the default mode it always had.
    #[cfg(unix)]
    pub(crate) fn owner_only(options: &mut OpenOptions, kind: LogSegmentKind) {
        use std::os::unix::fs::OpenOptionsExt;
        if kind.marked() {
            options.mode(0o600);
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn owner_only(_options: &mut OpenOptions, _kind: LogSegmentKind) {}

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
    file: Arc<File>,
    directory: Option<PathBuf>,
}

impl LogFlush {
    pub(crate) fn run(&self, durability: Durability) -> Result<(), NativeStorageError> {
        crate::write::failpoint("before-log-sync")?;
        match durability {
            Durability::Process => crate::sync_handle(&self.file),
            Durability::PowerLoss => self.file.sync_all(),
        }
        .map_err(NativeStorageError::io)?;
        if let Some(directory) = &self.directory {
            sync_directory(directory)?;
            if let Some(parent) = directory.parent() {
                sync_directory(parent)?;
            }
        }
        crate::write::failpoint("after-log-sync")
    }
}
