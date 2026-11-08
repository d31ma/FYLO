use std::path::{Path, PathBuf};

use fylo_vfs::{File, OpenOptions};

use super::{PositionedRead, SlotRecord};
use crate::NativeStorageError;

/// The two generation slots of a segment collection kept in a directory,
/// in its `state.slots` file.
///
/// A publication overwrites, in place and without a flush, the slot that does
/// not hold the newest valid generation. A torn write leaves the other slot,
/// so a reader always finds the previous generation at worst.
pub(crate) struct GenerationSlots {
    path: PathBuf,
}

impl GenerationSlots {
    const FILE_BYTES: usize = 2 * SlotRecord::ENCODED_BYTES;

    pub(crate) fn at(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
        }
    }

    /// The newest valid slot, or `None` when neither slot is valid.
    /// A missing slot file reads as neither slot valid: a layout migration
    /// moved it aside, and recovery finishes the migration.
    pub(crate) fn read(&self) -> Result<Option<SlotRecord>, NativeStorageError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(NativeStorageError::io(error)),
        };
        let (newest, _) = Self::newest(&Self::read_both(&file)?);
        Ok(newest)
    }

    /// Write `record` over the older slot.
    pub(crate) fn publish(&self, record: &SlotRecord) -> Result<(), NativeStorageError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(NativeStorageError::io)?;
        let (_, newest_index) = Self::newest(&Self::read_both(&file)?);
        let target = 1 - newest_index.unwrap_or(1);
        Self::write_at(
            &file,
            &record.encode(),
            (target * SlotRecord::ENCODED_BYTES) as u64,
        )
    }

    /// Create the slot file holding `record` in its first slot.
    pub(crate) fn create(&self, record: &SlotRecord) -> Result<(), NativeStorageError> {
        let mut bytes = vec![0_u8; Self::FILE_BYTES];
        bytes[..SlotRecord::ENCODED_BYTES].copy_from_slice(&record.encode());
        crate::write::durable_replace_owner_only(&self.path, &bytes)
    }

    fn read_both(file: &File) -> Result<[u8; Self::FILE_BYTES], NativeStorageError> {
        let mut bytes = [0_u8; Self::FILE_BYTES];
        PositionedRead::exact(file, &mut bytes, 0)?;
        Ok(bytes)
    }

    fn newest(bytes: &[u8]) -> (Option<SlotRecord>, Option<usize>) {
        let first = SlotRecord::decode(&bytes[..SlotRecord::ENCODED_BYTES]);
        let second = SlotRecord::decode(&bytes[SlotRecord::ENCODED_BYTES..]);
        match (first, second) {
            (Some(a), Some(b)) if Self::order(&b) > Self::order(&a) => (Some(b), Some(1)),
            (Some(a), _) => (Some(a), Some(0)),
            (None, Some(b)) => (Some(b), Some(1)),
            (None, None) => (None, None),
        }
    }

    /// Slots order by generation, then by where the active segment ends: a
    /// checkpoint publishes again at the same generation after appending its
    /// record or opening the next segment.
    fn order(slot: &SlotRecord) -> (u64, u64, u64) {
        (slot.generation, slot.segment, slot.length)
    }

    #[cfg(unix)]
    fn write_at(file: &File, bytes: &[u8], offset: u64) -> Result<(), NativeStorageError> {
        use std::os::unix::fs::FileExt;
        file.write_all_at(bytes, offset)
            .map_err(NativeStorageError::io)
    }

    #[cfg(not(unix))]
    fn write_at(file: &File, bytes: &[u8], offset: u64) -> Result<(), NativeStorageError> {
        use std::io::{Seek, SeekFrom, Write};
        let mut writer = file;
        writer
            .seek(SeekFrom::Start(offset))
            .and_then(|_| writer.write_all(bytes))
            .map_err(NativeStorageError::io)
    }
}
