use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fylo_vfs::{self as fs, File, OpenOptions};

use super::{
    GenerationSlots, MediumIdentity, PositionedRead, RecordPosition, SegmentHandle, SegmentMedium,
    SegmentPaths, SlotRecord,
};
use crate::write::{durable_replace_owner_only, ensure_directory, sync_directory};
use crate::{Durability, NativeRoot, NativeStorageError, NativeStorageErrorCode};

/// A segment collection kept as files in its collection directory's
/// `segments/` (ADR 0015 section 2): numbered `.seg` files, `state.slots`,
/// `locations.snapshot` and `locations.wal`.
///
/// Segment files, slots and location files are readable by the root owner
/// only (ADR 0015 section 10).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryMedium {
    paths: SegmentPaths,
}

impl DirectoryMedium {
    const MAX_SEGMENT_BYTES: u64 = 1024 * 1024 * 1024;
    const COMPACTION_PREFIX: &'static str = "compacting-";
    const COMPACTION_SUFFIX: &'static str = ".tmp";

    /// The medium of the segment collection in directory `collection`.
    pub(crate) fn of(collection: &Path) -> Self {
        Self {
            paths: SegmentPaths::of(collection),
        }
    }

    /// The medium of `collection`, as the segment code shares it.
    pub(crate) fn shared(collection: &Path) -> Arc<dyn SegmentMedium> {
        Arc::new(Self::of(collection))
    }

    fn compaction_path(&self) -> PathBuf {
        self.paths.directory().join(format!(
            "{}{}{}",
            Self::COMPACTION_PREFIX,
            crate::process_id(),
            Self::COMPACTION_SUFFIX
        ))
    }

    fn slots(&self) -> GenerationSlots {
        GenerationSlots::at(&self.paths.slots())
    }

    fn file_length(path: &Path) -> Result<u64, NativeStorageError> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(metadata.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, NativeStorageError> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }
}

impl SegmentMedium for DirectoryMedium {
    fn identity(&self) -> MediumIdentity {
        MediumIdentity::new(self.paths.directory().as_os_str())
    }

    fn max_segment_bytes(&self) -> u64 {
        Self::MAX_SEGMENT_BYTES
    }

    fn segment_numbers(&self) -> Result<Vec<u64>, NativeStorageError> {
        self.paths.segment_numbers()
    }

    fn segment_length(&self, number: u64) -> Result<u64, NativeStorageError> {
        Self::file_length(&self.paths.segment(number))
    }

    fn read_segment(&self, number: u64) -> Result<Option<Vec<u8>>, NativeStorageError> {
        Self::read_if_present(&self.paths.segment(number))
    }

    fn read_handle(&self, number: u64) -> Result<Arc<dyn SegmentHandle>, NativeStorageError> {
        let path = self.paths.segment(number);
        let file = File::open(&path)
            .map_err(NativeStorageError::io)
            .map_err(FileSegment::vanished)?;
        Ok(Arc::new(FileSegment { file, path }))
    }

    fn write_handle(
        &self,
        number: u64,
    ) -> Result<Option<Arc<dyn SegmentHandle>>, NativeStorageError> {
        let segment = FileSegment::open(&self.paths.segment(number))?;
        Ok(segment.map(|segment| Arc::new(segment) as Arc<dyn SegmentHandle>))
    }

    /// The next number is one above the highest segment file.
    fn create_segment(
        &self,
        opening: &dyn Fn(u64) -> Result<Vec<u8>, NativeStorageError>,
    ) -> Result<(u64, Arc<dyn SegmentHandle>), NativeStorageError> {
        let number = self.segment_numbers()?.last().copied().unwrap_or(0) + 1;
        let segment = FileSegment::create(&self.paths.segment(number), &opening(number)?, true)?;
        Ok((number, Arc::new(segment)))
    }

    /// A segment file has no state beyond its bytes.
    fn seal_segment(&self, _number: u64, _live_bytes: u64) -> Result<(), NativeStorageError> {
        Ok(())
    }

    fn remove_segment(&self, number: u64) -> Result<(), NativeStorageError> {
        match fs::remove_file(self.paths.segment(number)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) if cfg!(windows) && error.kind() == std::io::ErrorKind::PermissionDenied => {
                Ok(())
            }
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn segment_path(&self, number: u64) -> PathBuf {
        self.paths.segment(number)
    }

    fn create_compaction(&self) -> Result<Arc<dyn SegmentHandle>, NativeStorageError> {
        let path = self.compaction_path();
        let mut options = OpenOptions::new();
        FileSegment::owner_only(&mut options);
        // Chained into `open`: a WebAssembly host's builder returns a
        // copy, so flags set in a separate statement would be lost.
        let file = options
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(NativeStorageError::io)?;
        Ok(Arc::new(FileSegment { file, path }))
    }

    fn read_compaction(&self) -> Result<Vec<u8>, NativeStorageError> {
        fs::read(self.compaction_path()).map_err(NativeStorageError::io)
    }

    fn adopt_compaction(&self) -> Result<u64, NativeStorageError> {
        let number = self.segment_numbers()?.last().copied().unwrap_or(0) + 1;
        fs::rename(self.compaction_path(), self.paths.segment(number))
            .map_err(NativeStorageError::io)?;
        sync_directory(self.paths.directory())?;
        Ok(number)
    }

    fn remove_compactions(&self) -> Result<(), NativeStorageError> {
        for entry in fs::read_dir(self.paths.directory()).map_err(NativeStorageError::io)? {
            let entry = entry.map_err(NativeStorageError::io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(Self::COMPACTION_PREFIX) && name.ends_with(Self::COMPACTION_SUFFIX)
            {
                fs::remove_file(entry.path()).map_err(NativeStorageError::io)?;
            }
        }
        Ok(())
    }

    /// The slot file exists.
    fn has_slots(&self) -> Result<bool, NativeStorageError> {
        crate::path_exists_no_follow(&self.paths.slots())
    }

    /// The `segments/` directory.
    fn create_storage(&self, root: &NativeRoot) -> Result<(), NativeStorageError> {
        ensure_directory(root, self.paths.directory())
    }

    fn read_slot(&self) -> Result<Option<SlotRecord>, NativeStorageError> {
        self.slots().read()
    }

    fn publish_slot(&self, slot: &SlotRecord) -> Result<(), NativeStorageError> {
        self.slots().publish(slot)
    }

    fn create_slots(&self, slot: &SlotRecord) -> Result<(), NativeStorageError> {
        self.slots().create(slot)
    }

    fn sync_slots(&self) -> Result<(), NativeStorageError> {
        let slots = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.paths.slots())
            .map_err(NativeStorageError::io)?;
        crate::sync_handle(&slots).map_err(NativeStorageError::io)?;
        sync_directory(self.paths.directory())
    }

    fn read_location_snapshot(&self) -> Result<Vec<u8>, NativeStorageError> {
        Ok(Self::read_if_present(&self.paths.locations_snapshot())?.unwrap_or_default())
    }

    fn replace_location_snapshot(&self, bytes: &[u8]) -> Result<(), NativeStorageError> {
        durable_replace_owner_only(&self.paths.locations_snapshot(), bytes)
    }

    fn has_location_log(&self) -> bool {
        true
    }

    /// The location log carries every row after the snapshot, which
    /// therefore reaches no record position.
    fn read_location_checkpoint(&self) -> Result<(Vec<u8>, RecordPosition), NativeStorageError> {
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "a segment directory extends its location snapshot with its location log",
        ))
    }

    fn location_log_length(&self) -> Result<u64, NativeStorageError> {
        Self::file_length(&self.paths.locations_wal())
    }

    fn read_location_log(&self, start: u64, end: u64) -> Result<Vec<u8>, NativeStorageError> {
        let file = File::open(self.paths.locations_wal()).map_err(NativeStorageError::io)?;
        let size = usize::try_from(end - start).map_err(|_| {
            NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                "segment location log exceeds the address space",
            )
        })?;
        let mut bytes = vec![0_u8; size];
        PositionedRead::exact(&file, &mut bytes, start)?;
        Ok(bytes)
    }

    fn append_location_log(&self, bytes: &[u8]) -> Result<(), NativeStorageError> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.paths.locations_wal())
            .map_err(NativeStorageError::io)?;
        file.write_all(bytes).map_err(NativeStorageError::io)
    }

    fn truncate_location_log(&self, length: u64) -> Result<(), NativeStorageError> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.paths.locations_wal())
            .and_then(|file| file.set_len(length))
            .map_err(NativeStorageError::io)
    }

    fn reset_location_log(&self) -> Result<(), NativeStorageError> {
        durable_replace_owner_only(&self.paths.locations_wal(), b"")
    }

    fn sync_location_log(&self) -> Result<(), NativeStorageError> {
        let path = self.paths.locations_wal();
        if !crate::path_exists_no_follow(&path)? {
            return Ok(());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(NativeStorageError::io)?;
        crate::sync_handle(&file).map_err(NativeStorageError::io)
    }

    #[cfg(unix)]
    fn owner(&self) -> Result<(u32, u32), NativeStorageError> {
        use std::os::unix::fs::MetadataExt;
        let metadata =
            fs::symlink_metadata(self.paths.directory()).map_err(NativeStorageError::io)?;
        Ok((metadata.uid(), metadata.gid()))
    }
}

/// One open segment file, or a file collection's write-ahead log file.
#[derive(Debug)]
pub(crate) struct FileSegment {
    file: File,
    path: PathBuf,
}

impl FileSegment {
    /// Open the file for reading and writing, or `None` when it is missing.
    pub(crate) fn open(path: &Path) -> Result<Option<Self>, NativeStorageError> {
        match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => Ok(Some(Self {
                file,
                path: path.to_owned(),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    /// Create the file, which must not exist, holding `opening`. A document
    /// segment is `owner_only`; a log segment keeps the default mode it
    /// always had.
    pub(crate) fn create(
        path: &Path,
        opening: &[u8],
        owner_only: bool,
    ) -> Result<Self, NativeStorageError> {
        let mut options = OpenOptions::new();
        if owner_only {
            Self::owner_only(&mut options);
        }
        let mut file = options
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(NativeStorageError::io)?;
        file.write_all(opening).map_err(NativeStorageError::io)?;
        Ok(Self {
            file,
            path: path.to_owned(),
        })
    }

    #[cfg(unix)]
    fn owner_only(options: &mut OpenOptions) {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    #[cfg(not(unix))]
    fn owner_only(_options: &mut OpenOptions) {}

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
}

impl SegmentHandle for FileSegment {
    fn length(&self) -> Result<u64, NativeStorageError> {
        Ok(self.file.metadata().map_err(NativeStorageError::io)?.len())
    }

    fn read_at(&self, buffer: &mut [u8], offset: u64) -> Result<(), NativeStorageError> {
        PositionedRead::exact(&self.file, buffer, offset).map_err(Self::vanished)
    }

    /// The handle is not opened for appending, because Windows cannot
    /// truncate an append-only handle, and a failed append must be cut back
    /// off.
    fn write_at(&self, bytes: &[u8], offset: u64) -> Result<(), NativeStorageError> {
        let mut file = &self.file;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.write_all(bytes))
            .map_err(NativeStorageError::io)
    }

    fn truncate(&self, length: u64) -> Result<(), NativeStorageError> {
        self.file.set_len(length).map_err(NativeStorageError::io)
    }

    fn sync(&self, durability: Durability) -> Result<(), NativeStorageError> {
        match durability {
            Durability::Process => crate::sync_handle(&self.file),
            Durability::PowerLoss => self.file.sync_all(),
        }
        .map_err(NativeStorageError::io)
    }

    fn sync_created(&self) -> Result<(), NativeStorageError> {
        match self.path.parent() {
            Some(directory) => sync_directory(directory),
            None => Ok(()),
        }
    }
}
