use std::path::{Path, PathBuf};

use fylo_vfs::{File, OpenOptions};

use super::{LayoutId, PositionedRead, SlotRecord};
use crate::NativeStorageError;

/// The two generation slots of a segment collection kept in a directory,
/// in its `state.slots` file.
///
/// A publication overwrites, in place and without a flush, the slot that does
/// not hold the newest valid generation. A torn write leaves the other slot,
/// so a reader always finds the previous generation at worst.
///
/// After the two slots, the file holds the id of the layout (ADR 0027),
/// written once when the file is created. A file written before layout ids
/// ends after the slots, and reads as [`LayoutId::UNKNOWN`].
pub(crate) struct GenerationSlots {
    path: PathBuf,
}

impl GenerationSlots {
    const FILE_BYTES: usize = 2 * SlotRecord::ENCODED_BYTES;
    const WITH_LAYOUT_BYTES: usize = Self::FILE_BYTES + LayoutId::ENCODED_BYTES;

    pub(crate) fn at(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
        }
    }

    /// The newest valid slot, or `None` when neither slot is valid.
    /// A missing slot file reads as neither slot valid: a layout migration
    /// moved it aside, and recovery finishes the migration.
    pub(crate) fn read(&self) -> Result<Option<SlotRecord>, NativeStorageError> {
        Ok(self.read_in_layout()?.map(|(slot, _)| slot))
    }

    /// The newest valid slot with the id of the layout the file belongs to,
    /// read together in one `pread`.
    pub(crate) fn read_in_layout(
        &self,
    ) -> Result<Option<(SlotRecord, LayoutId)>, NativeStorageError> {
        let Some(file) = self.open_if_present()? else {
            return Ok(None);
        };
        let bytes = Self::read_with_layout(&file)?;
        let (newest, _) = Self::newest(&bytes[..Self::FILE_BYTES]);
        let layout = LayoutId::decode(&bytes[Self::FILE_BYTES..]);
        Ok(newest.map(|slot| (slot, layout)))
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

    /// Create the slot file holding `record` in its first slot, for the
    /// layout `layout`.
    pub(crate) fn create(
        &self,
        record: &SlotRecord,
        layout: LayoutId,
    ) -> Result<(), NativeStorageError> {
        let mut bytes = vec![0_u8; Self::WITH_LAYOUT_BYTES];
        bytes[..SlotRecord::ENCODED_BYTES].copy_from_slice(&record.encode());
        bytes[Self::FILE_BYTES..].copy_from_slice(&layout.encode());
        crate::write::durable_replace_owner_only(&self.path, &bytes)
    }

    fn open_if_present(&self) -> Result<Option<File>, NativeStorageError> {
        match File::open(&self.path) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(NativeStorageError::io(error)),
        }
    }

    fn read_both(file: &File) -> Result<[u8; Self::FILE_BYTES], NativeStorageError> {
        let mut bytes = [0_u8; Self::FILE_BYTES];
        PositionedRead::exact(file, &mut bytes, 0)?;
        Ok(bytes)
    }

    /// Both slots and as much of the layout block as the file holds: an
    /// older file ends after the slots.
    fn read_with_layout(file: &File) -> Result<Vec<u8>, NativeStorageError> {
        let mut bytes = vec![0_u8; Self::WITH_LAYOUT_BYTES];
        let read = PositionedRead::up_to(file, &mut bytes, 0)?;
        Self::require_slots(read)?;
        bytes.truncate(read);
        Ok(bytes)
    }

    fn require_slots(read: usize) -> Result<(), NativeStorageError> {
        if read < Self::FILE_BYTES {
            return Err(NativeStorageError::io(std::io::Error::from(
                std::io::ErrorKind::UnexpectedEof,
            )));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::GenerationSlots;
    use crate::segments::{LayoutId, SlotRecord};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "fylo-generation-slots-{label}-{}-{}",
                std::process::id(),
                crate::write::next_ttid_tick().unwrap()
            ));
            std::fs::create_dir_all(&directory).unwrap();
            Self(directory)
        }

        fn slots(&self) -> GenerationSlots {
            GenerationSlots::at(&self.0.join("state.slots"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn slot(generation: u64) -> SlotRecord {
        SlotRecord {
            generation,
            writing: false,
            segment: 1,
            length: 512,
            next_lsn: 3,
            records: 2,
            locations_length: 0,
            table_epoch: 9,
        }
    }

    #[test]
    fn a_created_file_names_its_layout() {
        let scratch = Scratch::new("created");
        let layout = LayoutId::fresh().unwrap();
        scratch.slots().create(&slot(1), layout).unwrap();
        assert_eq!(
            scratch.slots().read_in_layout().unwrap(),
            Some((slot(1), layout))
        );
    }

    #[test]
    fn a_publication_keeps_the_layout_id() {
        let scratch = Scratch::new("published");
        let layout = LayoutId::fresh().unwrap();
        scratch.slots().create(&slot(1), layout).unwrap();
        scratch.slots().publish(&slot(2)).unwrap();
        scratch.slots().publish(&slot(3)).unwrap();
        assert_eq!(
            scratch.slots().read_in_layout().unwrap(),
            Some((slot(3), layout))
        );
    }

    #[test]
    fn a_file_written_before_layout_ids_has_an_unknown_layout() {
        let scratch = Scratch::new("older");
        let mut bytes = vec![0_u8; 2 * SlotRecord::ENCODED_BYTES];
        bytes[..SlotRecord::ENCODED_BYTES].copy_from_slice(&slot(4).encode());
        std::fs::write(scratch.0.join("state.slots"), bytes).unwrap();
        assert_eq!(
            scratch.slots().read_in_layout().unwrap(),
            Some((slot(4), LayoutId::UNKNOWN))
        );
        scratch.slots().publish(&slot(5)).unwrap();
        assert_eq!(scratch.slots().read().unwrap(), Some(slot(5)));
    }

    #[test]
    fn a_file_shorter_than_two_slots_fails_to_read() {
        let scratch = Scratch::new("short");
        std::fs::write(scratch.0.join("state.slots"), [0_u8; 100]).unwrap();
        assert!(scratch.slots().read_in_layout().is_err());
    }

    #[test]
    fn a_missing_file_has_no_slot() {
        let scratch = Scratch::new("missing");
        assert_eq!(scratch.slots().read_in_layout().unwrap(), None);
    }
}
