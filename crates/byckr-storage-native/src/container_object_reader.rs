use std::io::{self, Read};
use std::ops::Range;

use byckr_format::{
    ContainerExtentKind, ContainerFormatError, ContainerObjectFrame, ContainerObjectLayout,
};
use sha2::{Digest, Sha256};

use crate::{
    ContainerError, ContainerFile, ContainerObject, ContainerObjectChunk, ContainerObjectError,
    ContainerObjectOpener,
};

/// Reads one large object from a `.vdbx` file chunk by chunk, with
/// positioned reads, holding one chunk in memory (ADR 0034 section 3).
///
/// Every chunk's checksum (and tag, when encrypted) is checked. A whole
/// read also checks the object's SHA-256 at the end; a range read reads
/// and checks only the chunks that cover it. Errors reach [`Read`] callers
/// as an [`io::Error`] that carries the [`ContainerObjectError`]; see
/// [`ContainerObjectError::from_read`].
pub struct ContainerObjectReader<'a> {
    container: &'a ContainerFile,
    object: &'a ContainerObject,
    layout: ContainerObjectLayout,
    opener: Option<ContainerObjectOpener>,
    frame: Vec<u8>,
    frame_hash: u64,
    chunk_count: u64,
    position: u64,
    end: u64,
    ready: Range<usize>,
    hasher: Option<Sha256>,
    checked_extent: Option<u64>,
}

impl<'a> ContainerObjectReader<'a> {
    const HEADER: usize = ContainerObjectFrame::HEADER_BYTES;

    /// A reader of the whole object, which checks its SHA-256 at the end.
    ///
    /// # Errors
    ///
    /// As [`Self::open_range`].
    pub fn open(
        container: &'a ContainerFile,
        object: &'a ContainerObject,
        file_key: Option<&[u8; 32]>,
    ) -> Result<Self, ContainerObjectError> {
        let mut reader = Self::open_range(container, object, file_key, 0..object.length)?;
        reader.hasher = Some(Sha256::new());
        Ok(reader)
    }

    /// A reader of the bytes `range` of the object, which reads only the
    /// chunks that cover it.
    ///
    /// # Errors
    ///
    /// Returns `RangeOutside`, `Locked` for an encrypted object without
    /// `file_key`, or `Damaged` when the record's fields do not fit the
    /// file.
    pub fn open_range(
        container: &'a ContainerFile,
        object: &'a ContainerObject,
        file_key: Option<&[u8; 32]>,
        range: Range<u64>,
    ) -> Result<Self, ContainerObjectError> {
        Self::require_range(object, &range)?;
        let opener = Self::opener_for(object, file_key)?;
        let layout = Self::layout_for(container, opener.is_some());
        Self::require_chunk_size(object, &layout)?;
        Ok(Self {
            container,
            object,
            layout,
            opener,
            frame: Vec::new(),
            frame_hash: object.frame_hash(),
            chunk_count: layout.chunk_count(object.length),
            position: range.start,
            end: range.end,
            ready: 0..0,
            hasher: None,
            checked_extent: None,
        })
    }

    fn require_range(
        object: &ContainerObject,
        range: &Range<u64>,
    ) -> Result<(), ContainerObjectError> {
        if range.start <= range.end && range.end <= object.length {
            return Ok(());
        }
        Err(ContainerObjectError::RangeOutside {
            start: range.start,
            end: range.end,
            length: object.length,
        })
    }

    fn opener_for(
        object: &ContainerObject,
        file_key: Option<&[u8; 32]>,
    ) -> Result<Option<ContainerObjectOpener>, ContainerObjectError> {
        match (object.salt_bytes()?, file_key) {
            (Some(salt), Some(key)) => Ok(Some(ContainerObjectOpener::new(key, &salt)?)),
            (Some(_), None) => Err(ContainerObjectError::Locked),
            (None, _) => Ok(None),
        }
    }

    fn layout_for(container: &ContainerFile, encrypted: bool) -> ContainerObjectLayout {
        let tag = u64::from(encrypted) * ContainerObjectOpener::TAG_BYTES as u64;
        ContainerObjectLayout::for_extent(container.superblock().extent_bytes(), tag)
    }

    fn require_chunk_size(
        object: &ContainerObject,
        layout: &ContainerObjectLayout,
    ) -> Result<(), ContainerObjectError> {
        if object.chunk_size == layout.chunk_bytes {
            return Ok(());
        }
        Err(ContainerObjectError::damaged(
            "the record's chunk size does not fit the file",
        ))
    }

    fn fill(&mut self) -> Result<(), ContainerObjectError> {
        if !self.ready.is_empty() {
            return Ok(());
        }
        if self.position >= self.end {
            return self.check_whole();
        }
        self.load_chunk(self.position / self.layout.chunk_bytes)
    }

    fn check_whole(&mut self) -> Result<(), ContainerObjectError> {
        let Some(hasher) = self.hasher.take() else {
            return Ok(());
        };
        if hasher.finalize()[..] == self.object.sha256_bytes()? {
            return Ok(());
        }
        Err(ContainerObjectError::damaged(
            "the file's SHA-256 does not match its record",
        ))
    }

    fn load_chunk(&mut self, index: u64) -> Result<(), ContainerObjectError> {
        let chunk = ContainerObjectChunk {
            index,
            count: self.chunk_count,
        };
        let plain = Self::to_usize(self.layout.chunk_length(self.object.length, index))?;
        let stored = plain + usize::from(self.opener.is_some()) * ContainerObjectOpener::TAG_BYTES;
        self.read_frame(index, stored)?;
        self.require_frame(chunk, stored)?;
        self.open_frame(chunk, plain)?;
        self.hash_chunk(plain);
        self.ready = self.ready_bytes(index, plain)?;
        Ok(())
    }

    fn read_frame(&mut self, index: u64, stored: usize) -> Result<(), ContainerObjectError> {
        let place = self.layout.place(index);
        let extent = self
            .object
            .extent_at(place.ordinal)
            .ok_or_else(|| ContainerObjectError::damaged("the record names too few extents"))?;
        self.check_extent(extent)?;
        self.frame.resize(Self::HEADER + stored, 0);
        self.container
            .read_payload(extent, place.offset, &mut self.frame)?;
        Ok(())
    }

    fn check_extent(&mut self, extent: u64) -> Result<(), ContainerObjectError> {
        if self.checked_extent == Some(extent) {
            return Ok(());
        }
        let header = self
            .container
            .read_extent_header_at(extent)
            .map_err(Self::header_error)?;
        if header.kind != ContainerExtentKind::Object
            || header.owner != self.object.target.collection
        {
            return Err(ContainerObjectError::NotFound);
        }
        self.checked_extent = Some(extent);
        Ok(())
    }

    fn header_error(error: ContainerError) -> ContainerObjectError {
        match error {
            ContainerError::Format(ContainerFormatError::BadMagic) => {
                ContainerObjectError::NotFound
            }
            other => ContainerObjectError::Container(other),
        }
    }

    fn require_frame(
        &self,
        chunk: ContainerObjectChunk,
        stored: usize,
    ) -> Result<(), ContainerObjectError> {
        let header = ContainerObjectFrame::read(&self.frame)?;
        if header.object != self.frame_hash {
            return Err(ContainerObjectError::NotFound);
        }
        let expected = ContainerObjectFrame {
            object: self.frame_hash,
            index: chunk.index,
            length: u32::try_from(stored)
                .map_err(|_| ContainerObjectError::damaged("a chunk is too long"))?,
            last: chunk.is_last(),
        };
        if header == expected {
            return Ok(());
        }
        Err(ContainerObjectError::damaged(
            "a chunk frame is not the one its place expects",
        ))
    }

    fn open_frame(
        &mut self,
        chunk: ContainerObjectChunk,
        plain: usize,
    ) -> Result<(), ContainerObjectError> {
        let Some(opener) = self.opener.as_ref() else {
            return Ok(());
        };
        let (buffer, tag) = self.frame[Self::HEADER..].split_at_mut(plain);
        let tag: &[u8; 16] = (&*tag)
            .try_into()
            .map_err(|_| ContainerObjectError::damaged("a chunk has no tag"))?;
        opener.open_in_place(&self.object.target, chunk, buffer, tag)
    }

    fn hash_chunk(&mut self, plain: usize) {
        if let Some(hasher) = self.hasher.as_mut() {
            hasher.update(&self.frame[Self::HEADER..Self::HEADER + plain]);
        }
    }

    fn ready_bytes(&self, index: u64, plain: usize) -> Result<Range<usize>, ContainerObjectError> {
        let chunk_start = index * self.layout.chunk_bytes;
        let skip = Self::to_usize(self.position - chunk_start)?;
        let until = Self::to_usize(self.end - chunk_start)?.min(plain);
        Ok(Self::HEADER + skip..Self::HEADER + until)
    }

    fn to_usize(value: u64) -> Result<usize, ContainerObjectError> {
        usize::try_from(value)
            .map_err(|_| ContainerObjectError::damaged("a size does not fit in memory"))
    }
}

impl Read for ContainerObjectReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.fill().map_err(io::Error::other)?;
        let count = self.ready.len().min(out.len());
        let from = self.ready.start;
        out[..count].copy_from_slice(&self.frame[from..from + count]);
        self.ready.start += count;
        self.position += count as u64;
        Ok(count)
    }
}
