use fylo_format::{ContainerCollectionHeader, ContainerDirectoryJournal};

use super::{BLOCK_BYTES, ContainerError, ContainerFile};

/// The directory journal (ADR 0016, ADR 0021 section 7): every write of a
/// collection block's static part goes to the journal first, flushed, and
/// then in place, so a write a power cut tears is finished from the
/// journal instead of leaving a block that fails its checksum and a file
/// that no longer opens.
impl ContainerFile {
    const JOURNAL_SLOT_BYTES: u64 = ContainerDirectoryJournal::SLOT_BYTES as u64;

    /// Where the journal's copy A starts in the directory extent's
    /// payload: just before the access slots, or at the end of the payload
    /// when the file is not protected. The collection blocks end there.
    pub(super) fn journal_offset(&self) -> u64 {
        Self::access_slot_offset(self.extent_bytes(), self.is_protected())
            - 2 * Self::JOURNAL_SLOT_BYTES
    }

    /// Write `block_bytes`, which start with the static part, over
    /// directory block `block`: the static part to the journal, flushed,
    /// then the bytes in place, flushed.
    ///
    /// # Errors
    ///
    /// Returns the read, write or flush failure.
    pub(super) fn write_static_part(
        &self,
        block: u32,
        block_bytes: &[u8],
    ) -> Result<(), ContainerError> {
        self.write_journal(
            block,
            &block_bytes[..ContainerCollectionHeader::ENCODED_BYTES],
        )?;
        self.write_block_in_place(block, block_bytes)
    }

    /// The static part the journal holds for `block`, when the newest copy
    /// names that block.
    ///
    /// # Errors
    ///
    /// Returns the read failure.
    pub(super) fn journaled_static_part(
        &self,
        block: u32,
    ) -> Result<Option<Vec<u8>>, ContainerError> {
        Ok(self
            .newest_journal()?
            .map(|(journal, _)| journal)
            .filter(|journal| journal.block == block)
            .map(|journal| journal.static_part))
    }

    /// Write the journaled `static_part` back over block `block` when this
    /// open may write; a reader keeps it in memory only.
    ///
    /// # Errors
    ///
    /// Returns the write or flush failure.
    pub(super) fn restore_static_part(
        &self,
        block: u32,
        static_part: &[u8],
    ) -> Result<(), ContainerError> {
        if !self.writable {
            return Ok(());
        }
        self.write_block_in_place(block, static_part)
    }

    /// Clear the journal in `payload`, a copy of the directory extent's
    /// payload: a sealed file never writes its directory again, and its
    /// blocks are renumbered, so a copy would name the wrong block.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` when the offset does not fit in memory.
    #[cfg(not(target_family = "wasm"))]
    pub(super) fn clear_journal(&self, payload: &mut [u8]) -> Result<(), ContainerError> {
        let start = usize::try_from(self.journal_offset())
            .map_err(|_| fylo_format::ContainerFormatError::Oversized)?;
        payload[start..start + 2 * ContainerDirectoryJournal::SLOT_BYTES].fill(0);
        Ok(())
    }

    fn write_journal(&self, block: u32, static_part: &[u8]) -> Result<(), ContainerError> {
        let newest = self.newest_journal()?;
        let newest_in_a = newest.as_ref().is_some_and(|(_, in_a)| *in_a);
        let journal = ContainerDirectoryJournal {
            generation: newest.map_or(1, |(journal, _)| journal.generation + 1),
            block,
            static_part: static_part.to_vec(),
        };
        self.write_payload(
            self.superblock.directory_extent,
            self.journal_offset() + u64::from(newest_in_a) * Self::JOURNAL_SLOT_BYTES,
            &journal.encode()?,
        )?;
        self.sync()?;
        Ok(())
    }

    fn newest_journal(&self) -> Result<Option<(ContainerDirectoryJournal, bool)>, ContainerError> {
        let slot = ContainerDirectoryJournal::SLOT_BYTES;
        let mut copies = vec![0_u8; 2 * slot];
        self.read_payload(
            self.superblock.directory_extent,
            self.journal_offset(),
            &mut copies,
        )?;
        Ok(ContainerDirectoryJournal::newest(
            &copies[..slot],
            &copies[slot..],
        ))
    }

    fn write_block_in_place(&self, block: u32, bytes: &[u8]) -> Result<(), ContainerError> {
        #[cfg(test)]
        if super::fault::check(super::ContainerFault::TornDirectoryBlock).is_err() {
            return self.write_torn(block, bytes);
        }
        self.write_payload(
            self.superblock.directory_extent,
            u64::from(block) * BLOCK_BYTES,
            bytes,
        )?;
        self.sync()?;
        Ok(())
    }

    /// Write only the first sector of `bytes` over block `block` and fail,
    /// as a power cut that tears the write leaves the file.
    #[cfg(test)]
    fn write_torn(&self, block: u32, bytes: &[u8]) -> Result<(), ContainerError> {
        self.write_payload(
            self.superblock.directory_extent,
            u64::from(block) * BLOCK_BYTES,
            &bytes[..super::ContainerFault::TORN_SECTOR_BYTES],
        )?;
        self.sync()?;
        Err(std::io::Error::other("injected torn directory block").into())
    }
}

#[cfg(test)]
mod tests;
