use super::document_entry::DocumentEntry;

/// Reads an entry's bytes front to back, refusing to run past the end.
pub(crate) struct EntryReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> EntryReader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let end = self.position.checked_add(length)?;
        let slice = self.bytes.get(self.position..end)?;
        self.position = end;
        Some(slice)
    }

    pub(crate) fn take_u32(&mut self) -> Option<u32> {
        let bytes = self.take(DocumentEntry::CHECKSUM_BYTES)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }
}
