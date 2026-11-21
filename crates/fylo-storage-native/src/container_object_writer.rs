use std::io::{self, Read, Write};

use fylo_format::{
    ContainerExtentKind, ContainerObjectFrame, ContainerObjectLayout, ContainerSuperblock,
};
use sha2::{Digest, Sha256};

use crate::{
    ContainerExtentAllocation, ContainerFile, ContainerObject, ContainerObjectChunk,
    ContainerObjectError, ContainerObjectOpener, ContainerObjectSealer, ContainerObjectTarget,
};

/// Writes one large object into a `.vdbx` file, chunk by chunk, holding
/// about one chunk in memory (ADR 0034 sections 3 and 5).
///
/// [`Self::begin`] takes the object's extents and writes their headers;
/// each full chunk is framed and written as it arrives; [`Self::finish`]
/// flushes and returns the record fields. The object exists only once the
/// caller has committed its record, which is the last step: a crash before
/// that leaves extents that no record names.
///
/// The caller holds the database's allocation lock across [`Self::begin`].
pub struct ContainerObjectWriter<'a> {
    container: &'a mut ContainerFile,
    object: ContainerObject,
    layout: ContainerObjectLayout,
    sealer: Option<ContainerObjectSealer>,
    hasher: Sha256,
    frame: Vec<u8>,
    frame_hash: u64,
    chunk_count: u64,
    next_index: u64,
    written: u64,
}

impl<'a> ContainerObjectWriter<'a> {
    const HEADER: usize = ContainerObjectFrame::HEADER_BYTES;

    /// Start an object of `length` bytes for `target`, encrypted under
    /// `file_key` when one is given. Sets the file's `large-objects`
    /// feature first, then takes every extent the object needs.
    ///
    /// # Errors
    ///
    /// Returns `TooLarge` over the limit and `TooManyRuns` when the free
    /// extents are too scattered, both before any chunk is written, or the
    /// container's failure.
    pub fn begin(
        container: &'a mut ContainerFile,
        target: ContainerObjectTarget,
        length: u64,
        file_key: Option<&[u8; 32]>,
    ) -> Result<Self, ContainerObjectError> {
        Self::require_size(length)?;
        let sealer = file_key.map(ContainerObjectSealer::new).transpose()?;
        let layout = Self::layout_for(container, sealer.is_some());
        container.add_incompat(ContainerSuperblock::INCOMPAT_LARGE_OBJECTS)?;
        let taken = Self::take_extents(container, target.collection, layout.extent_count(length))?;
        let mut object = Self::describe(target, length, &layout, &taken);
        object.salt = sealer
            .as_ref()
            .map(|sealer| ContainerObject::hex(&sealer.salt()));
        Self::require_runs(container, &object)?;
        Ok(Self {
            frame_hash: object.frame_hash(),
            frame: Self::empty_frame(&layout)?,
            chunk_count: layout.chunk_count(length),
            container,
            object,
            layout,
            sealer,
            hasher: Sha256::new(),
            next_index: 0,
            written: 0,
        })
    }

    /// Write the rest of the object from `source`, which must hold
    /// exactly the bytes still to come, and finish. On failure the
    /// object's extents are released.
    ///
    /// # Errors
    ///
    /// Returns `LengthMismatch` when `source` holds another length, or the
    /// read or container failure.
    pub fn copy_from(
        mut self,
        source: &mut impl Read,
    ) -> Result<ContainerObject, ContainerObjectError> {
        let left = self.object.length - self.written;
        let copied = io::copy(&mut source.take(left.saturating_add(1)), &mut self);
        self.finish_after(copied.map_err(ContainerObjectError::from_read))
    }

    /// Write the next bytes of the object.
    ///
    /// # Errors
    ///
    /// Returns `LengthMismatch` past the declared length, or the container
    /// or cipher failure.
    pub fn push(&mut self, mut bytes: &[u8]) -> Result<(), ContainerObjectError> {
        while !bytes.is_empty() {
            bytes = self.take_some(bytes)?;
        }
        Ok(())
    }

    /// Write the last chunk, flush every frame, and return the fields the
    /// object's record carries. The caller then commits that record.
    ///
    /// # Errors
    ///
    /// Returns `LengthMismatch` when fewer bytes than declared were
    /// written, or the write or flush failure. The extents stay taken; call
    /// [`Self::abandon`] instead to release them.
    pub fn finish(mut self) -> Result<ContainerObject, ContainerObjectError> {
        self.write_partial_chunk()?;
        self.require_complete()?;
        self.container.sync()?;
        self.object.sha256 = ContainerObject::hex(&self.hasher.finalize());
        Ok(self.object)
    }

    /// Give up the object and release its extents, which become free at
    /// the next checkpoint.
    ///
    /// # Errors
    ///
    /// Returns the container's failure.
    pub fn abandon(self) -> Result<(), ContainerObjectError> {
        Self::release_all(self.container, &self.object.extents())
    }

    fn finish_after(
        self,
        copied: Result<u64, ContainerObjectError>,
    ) -> Result<ContainerObject, ContainerObjectError> {
        if let Err(error) = copied {
            self.abandon()?;
            return Err(error);
        }
        self.finish()
    }

    fn require_size(length: u64) -> Result<(), ContainerObjectError> {
        if length > ContainerObject::MAX_BYTES {
            return Err(ContainerObjectError::TooLarge { length });
        }
        Ok(())
    }

    fn layout_for(container: &ContainerFile, encrypted: bool) -> ContainerObjectLayout {
        let tag = u64::from(encrypted) * ContainerObjectOpener::TAG_BYTES as u64;
        ContainerObjectLayout::for_extent(container.superblock().extent_bytes(), tag)
    }

    fn take_extents(
        container: &mut ContainerFile,
        owner: u32,
        count: u64,
    ) -> Result<Vec<ContainerExtentAllocation>, ContainerObjectError> {
        let mut taken = Vec::new();
        for _ in 0..count {
            match container.allocate(ContainerExtentKind::Object, owner) {
                Ok(allocation) => taken.push(allocation),
                Err(error) => return Self::undo_take(container, &taken, error.into()),
            }
        }
        Ok(taken)
    }

    fn undo_take(
        container: &mut ContainerFile,
        taken: &[ContainerExtentAllocation],
        error: ContainerObjectError,
    ) -> Result<Vec<ContainerExtentAllocation>, ContainerObjectError> {
        let extents: Vec<u64> = taken.iter().map(|allocation| allocation.index).collect();
        Self::release_all(container, &extents)?;
        Err(error)
    }

    fn describe(
        target: ContainerObjectTarget,
        length: u64,
        layout: &ContainerObjectLayout,
        taken: &[ContainerExtentAllocation],
    ) -> ContainerObject {
        let extents: Vec<u64> = taken.iter().map(|allocation| allocation.index).collect();
        ContainerObject {
            target,
            length,
            sha256: String::new(),
            chunk_size: layout.chunk_bytes,
            runs: ContainerObject::runs_from(&extents),
            salt: None,
            stamp: taken.first().map_or(0, |allocation| allocation.sequence),
        }
    }

    fn require_runs(
        container: &mut ContainerFile,
        object: &ContainerObject,
    ) -> Result<(), ContainerObjectError> {
        if object.runs.len() <= ContainerObject::MAX_RUNS {
            return Ok(());
        }
        Self::release_all(container, &object.extents())?;
        Err(ContainerObjectError::TooManyRuns {
            runs: object.runs.len(),
        })
    }

    fn release_all(
        container: &mut ContainerFile,
        extents: &[u64],
    ) -> Result<(), ContainerObjectError> {
        for extent in extents {
            container.release(*extent)?;
        }
        Ok(())
    }

    fn empty_frame(layout: &ContainerObjectLayout) -> Result<Vec<u8>, ContainerObjectError> {
        let mut frame = Vec::with_capacity(Self::to_usize(layout.slot_bytes)?);
        frame.resize(Self::HEADER, 0);
        Ok(frame)
    }

    fn take_some<'b>(&mut self, bytes: &'b [u8]) -> Result<&'b [u8], ContainerObjectError> {
        let room = self.room(bytes.len())?;
        let (now, rest) = bytes.split_at(room.min(bytes.len()));
        self.frame.extend_from_slice(now);
        self.written += now.len() as u64;
        self.write_full_chunk()?;
        Ok(rest)
    }

    fn room(&self, wanted: usize) -> Result<usize, ContainerObjectError> {
        let room = self.chunk_length()? - self.filled();
        if room == 0 {
            return Err(ContainerObjectError::LengthMismatch {
                declared: self.object.length,
                written: self.written + wanted as u64,
            });
        }
        Ok(room)
    }

    fn chunk_length(&self) -> Result<usize, ContainerObjectError> {
        Self::to_usize(
            self.layout
                .chunk_length(self.object.length, self.next_index),
        )
    }

    fn filled(&self) -> usize {
        self.frame.len() - Self::HEADER
    }

    fn write_full_chunk(&mut self) -> Result<(), ContainerObjectError> {
        if self.filled() < self.chunk_length()? {
            return Ok(());
        }
        self.write_chunk()
    }

    fn write_partial_chunk(&mut self) -> Result<(), ContainerObjectError> {
        if self.filled() == 0 {
            return Ok(());
        }
        self.write_chunk()
    }

    fn write_chunk(&mut self) -> Result<(), ContainerObjectError> {
        let chunk = ContainerObjectChunk {
            index: self.next_index,
            count: self.chunk_count,
        };
        self.hasher.update(&self.frame[Self::HEADER..]);
        self.seal_frame(chunk)?;
        let header = ContainerObjectFrame {
            object: self.frame_hash,
            index: chunk.index,
            length: u32::try_from(self.filled())
                .map_err(|_| ContainerObjectError::damaged("a chunk is too long"))?,
            last: chunk.is_last(),
        };
        header.write_header(&mut self.frame)?;
        let place = self.layout.place(chunk.index);
        let extent = self
            .object
            .extent_at(place.ordinal)
            .ok_or_else(|| ContainerObjectError::damaged("a chunk has no extent"))?;
        self.container
            .write_payload(extent, place.offset, &self.frame)?;
        self.frame.truncate(Self::HEADER);
        self.next_index += 1;
        Ok(())
    }

    fn seal_frame(&mut self, chunk: ContainerObjectChunk) -> Result<(), ContainerObjectError> {
        let Some(sealer) = self.sealer.as_mut() else {
            return Ok(());
        };
        let tag =
            sealer.seal_in_place(&self.object.target, chunk, &mut self.frame[Self::HEADER..])?;
        self.frame.extend_from_slice(&tag);
        Ok(())
    }

    fn require_complete(&self) -> Result<(), ContainerObjectError> {
        if self.written == self.object.length && self.next_index == self.chunk_count {
            return Ok(());
        }
        Err(ContainerObjectError::LengthMismatch {
            declared: self.object.length,
            written: self.written,
        })
    }

    fn to_usize(value: u64) -> Result<usize, ContainerObjectError> {
        usize::try_from(value)
            .map_err(|_| ContainerObjectError::damaged("a size does not fit in memory"))
    }
}

impl Write for ContainerObjectWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.push(bytes).map_err(io::Error::other)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
