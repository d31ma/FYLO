use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fylo_format::{LogRecord, LogRecordKind, LogScan, LogSegmentHeader};
use fylo_vfs::{self as fs, File, OpenOptions};

use super::super::{json_error, sync_directory};
use super::checkpoint_manifest::CheckpointManifest;
use crate::{Durability, LogPosition, NativeStorageError, NativeStorageErrorCode};

/// The open write-ahead log segment of one collection.
///
/// There is one segment at a time. A checkpoint removes it, and the next
/// commit creates a new one whose first record is a checkpoint.
#[derive(Debug)]
pub(crate) struct LogSegment {
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
        let file = match OpenOptions::new().read(true).append(true).open(path) {
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
            file: Arc::new(file),
            path: path.to_owned(),
            nonce: header.map_or(0, |header| header.sequence),
            length,
            next_lsn: position.next_lsn,
            records: position.records,
            fresh: false,
        }))
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
        let nonce = Self::random_nonce()?;
        let header = LogSegmentHeader {
            sequence: nonce,
            first_lsn: Self::FIRST_LSN,
        };
        let manifest = serde_json::to_vec(checkpoint).map_err(|error| json_error(&error))?;
        let record = LogRecord {
            lsn: Self::FIRST_LSN,
            kind: LogRecordKind::Checkpoint,
            generation,
            manifest,
            blobs: Vec::new(),
        };
        let mut bytes = header.encode().to_vec();
        bytes.extend(Self::encode(&record)?);
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(path)
            .map_err(NativeStorageError::io)?;
        file.write_all(&bytes).map_err(NativeStorageError::io)?;
        Ok(Self {
            file: Arc::new(file),
            path: path.to_owned(),
            nonce,
            length: bytes.len() as u64,
            next_lsn: Self::FIRST_LSN + 1,
            records: 0,
            fresh: true,
        })
    }

    /// Append encoded records. A failed append is cut back off, so nothing
    /// unacknowledged stays in the log.
    pub(crate) fn append(&mut self, bytes: &[u8], records: u64) -> Result<(), NativeStorageError> {
        crate::write::failpoint("before-log-append")?;
        let outcome = (&*self.file)
            .write_all(bytes)
            .map_err(NativeStorageError::io)
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

    /// Encode one record, refusing a body the format cannot hold.
    pub(crate) fn encode(record: &LogRecord) -> Result<Vec<u8>, NativeStorageError> {
        record.encode().map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                format!("write-ahead log record cannot be encoded: {error}"),
            )
        })
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
