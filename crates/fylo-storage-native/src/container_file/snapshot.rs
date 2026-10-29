//! Derived snapshots in a `.fxdb` container (ADR 0016 sections 3 and 5).
//!
//! A collection's location snapshot and prefix-index snapshot each live in a
//! run of consecutive extents of kind `locations` or `index`, which the
//! collection's checkpoint slot names with a [`ContainerSnapshotRef`]: the
//! first extent, the run's length, the snapshot's length and its SHA-256.
//! The bytes fill the payloads in order, each extent full but the last.
//!
//! In an encrypted file the snapshot is sealed for its collection, kind and
//! the checkpoint that wrote it, and the reference's length and hash are
//! those of the sealed bytes, so `fylo verify` checks them without the key.
//!
//! A snapshot is derived, never a source of truth: a reader that finds the
//! run damaged, reused or of the wrong length rebuilds it by replaying the
//! collection's segments, so every check here fails closed rather than
//! guessing.

use fylo_format::{
    ContainerExtentHeader, ContainerExtentKind, ContainerFormatError, ContainerSnapshotRef,
};
use sha2::{Digest, Sha256};

use super::{ContainerError, ContainerFile, read_exact_at, write_extent_header};
use crate::ContainerSealContext;
use crate::container_allocator::{ContainerAllocatorError, ContainerExtentAllocation};

impl ContainerFile {
    /// Write a collection's derived snapshot into a fresh run of consecutive
    /// extents, flush it, and return the reference its checkpoint slot
    /// records. An empty snapshot still takes one extent, so the reference
    /// always names a run. `checkpoint` is the number of the checkpoint
    /// slot that will name it; an encrypted file seals the snapshot for it.
    ///
    /// The previous snapshot is left alone. The caller records the returned
    /// reference with [`Self::record_checkpoint`], then gives the previous
    /// run back with [`Self::release_snapshot`]; its extents are reused only
    /// after the next [`Self::checkpoint`], so a reader still holding the old
    /// reference never sees them overwritten before a map shows them free.
    ///
    /// # Errors
    ///
    /// Returns `ReadOnly` for a reader, `InvalidField` for a kind other than
    /// `Locations` or `Index`, `Oversized` for a snapshot longer than the
    /// format can name, `Cipher(Random)` when no nonce can be drawn, or the
    /// write, resize or flush failure. On a failure
    /// the run is released, so it is reused after the next checkpoint.
    pub fn write_snapshot(
        &mut self,
        collection: u32,
        kind: ContainerExtentKind,
        checkpoint: u64,
        bytes: &[u8],
    ) -> Result<ContainerSnapshotRef, ContainerError> {
        self.require_writable()?;
        require_snapshot_kind(kind)?;
        let sealed = match self.cipher() {
            Some(cipher) => Some(cipher.seal(
                ContainerSealContext::Snapshot {
                    collection,
                    checkpoint,
                    kind,
                },
                bytes,
            )?),
            None => None,
        };
        let bytes = sealed.as_deref().unwrap_or(bytes);
        let capacity = self.payload_capacity();
        let length = bytes.len() as u64;
        let extents = length.div_ceil(capacity).max(1);
        let extent_count = u32::try_from(extents).map_err(|_| ContainerFormatError::Oversized)?;
        let run = usize::try_from(extents).map_err(|_| ContainerFormatError::Oversized)?;
        let chunk = usize::try_from(capacity).map_err(|_| ContainerFormatError::Oversized)?;
        let allocations = self.allocator.allocate_run(kind, collection, run);
        if let Err(error) = self.fill_run(&allocations, kind, collection, bytes, chunk) {
            for allocation in &allocations {
                self.allocator.release(allocation.index)?;
            }
            return Err(error);
        }
        // A written snapshot never changes: the next one goes into a new
        // run. Sealing says so in the map, and its live bytes are the bytes
        // the extent holds, so `fylo inspect` counts the snapshot's size per
        // kind. A sealed extent is one a shrink may relocate; a snapshot
        // extent cannot be moved alone, because its reference names a run,
        // so a shrink rewrites the snapshot instead.
        for (position, allocation) in allocations.iter().enumerate() {
            let start = position as u64 * capacity;
            let live = length.saturating_sub(start).min(capacity);
            self.allocator.seal(allocation.index, live)?;
        }
        Ok(ContainerSnapshotRef {
            first_extent: allocations[0].index,
            extent_count,
            length,
            sha256: Sha256::digest(bytes).into(),
        })
    }

    /// Write the run's headers and payloads, growing the file first if the
    /// run reaches past its end, and flush.
    fn fill_run(
        &self,
        allocations: &[ContainerExtentAllocation],
        kind: ContainerExtentKind,
        collection: u32,
        bytes: &[u8],
        chunk: usize,
    ) -> Result<(), ContainerError> {
        if allocations.iter().any(|allocation| allocation.grows_file) {
            self.file()
                .set_len(self.allocator.extent_count() * self.extent_bytes())?;
        }
        for allocation in allocations {
            write_extent_header(
                self.file(),
                self.extent_bytes(),
                *allocation,
                kind,
                collection,
            )?;
        }
        for (allocation, part) in allocations.iter().zip(bytes.chunks(chunk)) {
            self.write_payload(allocation.index, 0, part)?;
        }
        self.sync()?;
        Ok(())
    }

    /// Read a collection's derived snapshot and check it against its
    /// reference, opening it with the key of an encrypted file. Works on
    /// readers. `checkpoint` is the number of the slot that names it, and
    /// `max_bytes` bounds the stored bytes, which in an encrypted file are
    /// the snapshot plus 28.
    ///
    /// The reference is checked before anything is allocated: its length
    /// against `max_bytes` and against what the run can hold, and the run
    /// against the file. Each extent's header must name the expected kind
    /// and collection and its own index. The reference carries no sequence
    /// numbers, so an extent freed and taken again by the same collection
    /// for the same kind passes the header check; the SHA-256 catches it.
    ///
    /// # Errors
    ///
    /// Returns `Oversized` when the snapshot is longer than `max_bytes`,
    /// `InvalidField` for a kind other than `Locations` or `Index`, a length
    /// the run cannot hold or an extent that holds something else,
    /// `Allocator(OutOfRange)` for a run outside the file,
    /// `ChecksumMismatch` when the bytes do not hash to the recorded
    /// SHA-256, `Cipher(Unauthenticated)` when the sealed bytes were sealed
    /// for another place, a header decoding error, or the read failure. All
    /// of them report `EFXDB_CORRUPT`, and the caller rebuilds the snapshot.
    /// A locked encrypted file gives `Locked`.
    pub fn read_snapshot(
        &self,
        collection: u32,
        kind: ContainerExtentKind,
        checkpoint: u64,
        snapshot: &ContainerSnapshotRef,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ContainerError> {
        require_snapshot_kind(kind)?;
        if self.is_locked() {
            return Err(ContainerError::Locked);
        }
        if snapshot.length > max_bytes {
            return Err(ContainerFormatError::Oversized.into());
        }
        let capacity = self.payload_capacity();
        let extents = u64::from(snapshot.extent_count);
        if extents == 0
            || extents
                .checked_mul(capacity)
                .is_none_or(|holds| snapshot.length > holds)
        {
            return Err(ContainerFormatError::InvalidField.into());
        }
        let run = self.snapshot_run(snapshot)?;
        let length =
            usize::try_from(snapshot.length).map_err(|_| ContainerFormatError::Oversized)?;
        let chunk = usize::try_from(capacity).map_err(|_| ContainerFormatError::Oversized)?;
        for extent in run.clone() {
            self.expect_snapshot_extent(extent, Some((kind, collection)))?;
        }
        let mut bytes = vec![0_u8; length];
        for (extent, part) in run.zip(bytes.chunks_mut(chunk)) {
            self.read_payload(extent, 0, part)?;
        }
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != snapshot.sha256 {
            return Err(ContainerFormatError::ChecksumMismatch.into());
        }
        match self.cipher() {
            Some(cipher) => Ok(cipher.open(
                ContainerSealContext::Snapshot {
                    collection,
                    checkpoint,
                    kind,
                },
                &bytes,
            )?),
            None => Ok(bytes),
        }
    }

    /// Release every extent of a snapshot's run, once a newer checkpoint
    /// slot no longer names it. The extents are reused only after the next
    /// [`Self::checkpoint`].
    ///
    /// Every header is checked before anything is released: each must be a
    /// `locations` or `index` extent at its own index, all of one kind and
    /// collection, so a damaged reference can never free a segment.
    ///
    /// # Errors
    ///
    /// Returns `ReadOnly` for a reader, `Allocator(OutOfRange)` for a run
    /// outside the file, `InvalidField` for an extent that is not part of a
    /// snapshot, `Allocator(NotInUse)` for one already released, or the read
    /// failure.
    pub fn release_snapshot(
        &mut self,
        snapshot: &ContainerSnapshotRef,
    ) -> Result<(), ContainerError> {
        self.require_writable()?;
        let run = self.snapshot_run(snapshot)?;
        let mut owner = None;
        for extent in run.clone() {
            let header = self.expect_snapshot_extent(extent, owner)?;
            owner = Some((header.kind, header.owner));
        }
        for extent in run {
            self.allocator.release(extent)?;
        }
        Ok(())
    }

    /// The extents of a snapshot's run, checked to lie inside the file and
    /// past the superblock region.
    fn snapshot_run(
        &self,
        snapshot: &ContainerSnapshotRef,
    ) -> Result<std::ops::Range<u64>, ContainerError> {
        let end = snapshot
            .first_extent
            .checked_add(u64::from(snapshot.extent_count));
        match end {
            Some(end) if snapshot.first_extent >= 1 && end <= self.allocator.extent_count() => {
                Ok(snapshot.first_extent..end)
            }
            _ => Err(ContainerAllocatorError::OutOfRange.into()),
        }
    }

    /// Read an extent's header and check it is part of a snapshot at its own
    /// index, of the given kind and collection when one is given.
    fn expect_snapshot_extent(
        &self,
        extent: u64,
        expected: Option<(ContainerExtentKind, u32)>,
    ) -> Result<ContainerExtentHeader, ContainerError> {
        let mut bytes = [0_u8; ContainerExtentHeader::ENCODED_BYTES];
        read_exact_at(self.file(), &mut bytes, extent * self.extent_bytes())?;
        let header = ContainerExtentHeader::decode(&bytes)?;
        let matches = expected.map_or_else(
            || require_snapshot_kind(header.kind).is_ok(),
            |owner| (header.kind, header.owner) == owner,
        );
        if header.index == extent && matches {
            Ok(header)
        } else {
            Err(ContainerFormatError::InvalidField.into())
        }
    }
}

/// Only the derived snapshot kinds live in a snapshot run.
fn require_snapshot_kind(kind: ContainerExtentKind) -> Result<(), ContainerFormatError> {
    if matches!(
        kind,
        ContainerExtentKind::Locations | ContainerExtentKind::Index
    ) {
        Ok(())
    } else {
        Err(ContainerFormatError::InvalidField)
    }
}

#[cfg(test)]
mod tests;
