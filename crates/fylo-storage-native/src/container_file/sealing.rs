//! Writing a sealed `.vdbx` file (ADR 0016 sections 8 and 11): `fylo pack
//! --sealed`.
//!
//! The source is a plain, unsealed container that no one writes, such as the
//! staging file a pack has just built and checked. Its extents in use are
//! copied in order into an image with no free extents: each keeps its
//! kind, owner and sequence number and takes the next index, so the
//! headers, the superblock and the slots that name extents by index are
//! rewritten to the new numbers, and a new map describing exactly those
//! extents goes at the end. Locations name segments by sequence number,
//! which does not change, so no record is touched.
//!
//! The image is then cut into 64 KiB blocks, each compressed on its own at
//! zstd level 19 ([`crate::ZSTD_SEALED_LEVEL`]), or kept as is when that
//! would not shrink it, and, with a secret, sealed with the block number
//! as associated data. A block of zeros, as most of a partly filled
//! extent's tail is, stores nothing. The blocks of each extent are
//! compressed on every core, then written in order.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use fylo_format::{
    ContainerCheckpointSlot, ContainerCollectionHeader, ContainerExtentHeader, ContainerExtentKind,
    ContainerExtentMapPage, ContainerExtentState, ContainerFormatError, ContainerGenerationSlot,
    ContainerSealedBlock, ContainerSealedHeader, ContainerSnapshotRef, ContainerSuperblock,
};
use fylo_vfs::{File, OpenOptions};

use super::{
    BLOCK_BYTES, ContainerError, ContainerFile, ContainerSealedReport, PAGE_BYTES, map_pages_for,
    write_all_at,
};
use crate::container_allocator::ContainerAllocatorError;
use crate::{ContainerCipher, ContainerFrameCodec, ContainerSealContext, ZSTD_SEALED_LEVEL};

impl ContainerFile {
    /// Write this container as a new sealed file at `out`, which must not
    /// exist: a read-only, block-compressed image with no free extents,
    /// encrypted with a key derived from `secret` when one is given. The
    /// file is flushed before this returns; the caller flushes its
    /// directory.
    ///
    /// This container must be plain, unsealed and checkpointed, and must
    /// not be written meanwhile.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a container that is encrypted, sealed or
    /// has changes no checkpoint holds, `Allocator(InconsistentMap)` when a
    /// slot names an extent the map does not show in use, the failure to
    /// draw a salt or a nonce, or the creation, read or write failure.
    pub fn write_sealed(
        &self,
        out: &Path,
        secret: Option<&[u8]>,
    ) -> Result<ContainerSealedReport, ContainerError> {
        if self.is_encrypted() || self.image.is_some() || self.allocator.has_unsaved_changes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only a plain, unsealed and checkpointed container can be sealed",
            )
            .into());
        }
        let plan = Renumbering::of(self);
        let cipher = match secret {
            Some(secret) => {
                let salt = ContainerCipher::new_salt()?;
                Some((ContainerCipher::derive(secret, &salt), salt))
            }
            None => None,
        };
        let mut superblock = plan.superblock(&self.superblock)?;
        if let Some((cipher, salt)) = &cipher {
            superblock.incompat |= ContainerSuperblock::INCOMPAT_ENCRYPTED;
            superblock.key_salt = *salt;
            superblock.key_check = cipher.key_check();
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(out)?;
        let mut writer = BlockWriter {
            file: &file,
            cipher: cipher.as_ref().map(|(cipher, _)| cipher),
            position: ContainerSealedHeader::DATA_OFFSET,
            blocks: Vec::new(),
        };
        // The superblock region is read from the file's own copies, so its
        // blocks are zeros.
        let mut extent = vec![
            0_u8;
            usize::try_from(self.extent_bytes())
                .map_err(|_| ContainerFormatError::Oversized)?
        ];
        writer.push(&extent)?;
        for index in 1..plan.extent_count() {
            self.image_extent(&plan, index, &mut extent)?;
            writer.push(&extent)?;
        }
        let image_bytes = plan.extent_count() * self.extent_bytes();
        writer.finish(&superblock, image_bytes)
    }

    /// Fill `extent` with what extent `index` of the sealed image holds.
    fn image_extent(
        &self,
        plan: &Renumbering,
        index: u64,
        extent: &mut [u8],
    ) -> Result<(), ContainerError> {
        extent.fill(0);
        let Some(source) = plan.source(index) else {
            let header = ContainerExtentHeader {
                kind: ContainerExtentKind::Map,
                owner: 0,
                sequence: plan.map_sequence(index),
                index,
            };
            extent[..ContainerExtentHeader::ENCODED_BYTES].copy_from_slice(&header.encode());
            let pages_per_extent = self.payload_capacity() / PAGE_BYTES;
            let first_page = (index - plan.map_first) * pages_per_extent;
            for (page, encoded) in plan
                .pages
                .iter()
                .enumerate()
                .skip(usize::try_from(first_page).map_err(|_| ContainerFormatError::Oversized)?)
            {
                let within = page as u64 - first_page;
                if within >= pages_per_extent {
                    break;
                }
                let at =
                    usize::try_from(ContainerExtentHeader::PAYLOAD_OFFSET + within * PAGE_BYTES)
                        .map_err(|_| ContainerFormatError::Oversized)?;
                extent[at..at + ContainerExtentMapPage::ENCODED_BYTES]
                    .copy_from_slice(&encoded.encode()?);
            }
            return Ok(());
        };
        self.read_at(extent, source * self.extent_bytes())?;
        let mut header = ContainerExtentHeader::decode(extent)?;
        if header.index != source {
            return Err(ContainerFormatError::InvalidField.into());
        }
        header.index = index;
        extent[..ContainerExtentHeader::ENCODED_BYTES].copy_from_slice(&header.encode());
        if header.kind == ContainerExtentKind::Directory {
            let payload = usize::try_from(ContainerExtentHeader::PAYLOAD_OFFSET)
                .map_err(|_| ContainerFormatError::Oversized)?;
            let block =
                usize::try_from(BLOCK_BYTES).map_err(|_| ContainerFormatError::Oversized)?;
            for bytes in extent[payload..].chunks_exact_mut(block) {
                if bytes.iter().all(|byte| *byte == 0) {
                    break;
                }
                plan.renumber_block(bytes)?;
            }
        }
        Ok(())
    }
}

/// Where each extent of the source goes in the sealed image.
struct Renumbering {
    /// The source extents kept, in order: image extent `n` is `kept[n - 1]`.
    kept: Vec<u64>,
    /// Image index by source index.
    index_of: BTreeMap<u64, u64>,
    /// The image's first map extent; the map runs to the end.
    map_first: u64,
    /// Map extents in the image.
    map_extents: u64,
    /// The sequence number the first map extent takes.
    map_sequence: u64,
    /// The image's map.
    pages: Vec<ContainerExtentMapPage>,
}

impl Renumbering {
    fn of(source: &ContainerFile) -> Self {
        let mut states = Vec::new();
        let mut kept = Vec::new();
        for page in source.allocator.map_pages() {
            for (offset, state) in page.entries.into_iter().enumerate() {
                let kind = match state {
                    ContainerExtentState::Free => continue,
                    ContainerExtentState::Owned { kind, .. }
                    | ContainerExtentState::Sealed { kind, .. } => kind,
                };
                if kind != ContainerExtentKind::Map {
                    kept.push(page.first_extent + offset as u64);
                    states.push(state);
                }
            }
        }
        let pages_per_extent = source.payload_capacity() / PAGE_BYTES;
        let mut map_extents = 1_u64;
        while map_extents * pages_per_extent < map_pages_for(1 + kept.len() as u64 + map_extents) {
            map_extents += 1;
        }
        let map_first = 1 + kept.len() as u64;
        let map_sequence = source.allocator.next_sequence();
        for extent in 0..map_extents {
            states.push(ContainerExtentState::Owned {
                kind: ContainerExtentKind::Map,
                owner: 0,
                sequence: map_sequence + extent,
            });
        }
        let pages = states
            .chunks(ContainerExtentMapPage::ENTRIES_PER_PAGE)
            .enumerate()
            .map(|(page, chunk)| ContainerExtentMapPage {
                first_extent: 1 + (page * ContainerExtentMapPage::ENTRIES_PER_PAGE) as u64,
                entries: chunk.to_vec(),
            })
            .collect();
        let index_of = kept
            .iter()
            .enumerate()
            .map(|(position, source)| (*source, position as u64 + 1))
            .collect();
        Self {
            kept,
            index_of,
            map_first,
            map_extents,
            map_sequence,
            pages,
        }
    }

    /// Extents in the image, the superblock region's included.
    fn extent_count(&self) -> u64 {
        self.map_first + self.map_extents
    }

    /// The source extent image extent `index` copies, or `None` for one of
    /// the new map's.
    fn source(&self, index: u64) -> Option<u64> {
        usize::try_from(index - 1)
            .ok()
            .and_then(|position| self.kept.get(position))
            .copied()
    }

    fn map_sequence(&self, index: u64) -> u64 {
        self.map_sequence + (index - self.map_first)
    }

    /// The image index of source extent `extent`.
    fn index(&self, extent: u64) -> Result<u64, ContainerError> {
        self.index_of
            .get(&extent)
            .copied()
            .ok_or_else(|| ContainerAllocatorError::InconsistentMap.into())
    }

    /// The sealed image's superblock: the source's, renumbered and marked
    /// sealed. No key yet; the caller adds one.
    fn superblock(
        &self,
        source: &ContainerSuperblock,
    ) -> Result<ContainerSuperblock, ContainerError> {
        Ok(ContainerSuperblock {
            generation: source.generation + 1,
            extent_count: self.extent_count(),
            next_extent_sequence: self.map_sequence + self.map_extents,
            directory_extent: self.index(source.directory_extent)?,
            map_first_extent: self.map_first,
            map_extent_count: u32::try_from(self.map_extents)
                .map_err(|_| ContainerFormatError::Oversized)?,
            incompat: (source.incompat & !ContainerSuperblock::INCOMPAT_ENCRYPTED)
                | ContainerSuperblock::INCOMPAT_SEALED,
            key_salt: [0; 32],
            key_check: [0; 16],
            ..source.clone()
        })
    }

    /// Renumber the slots of one collection block: the newest of each pair
    /// goes into the first slot with its extents renumbered, and the second
    /// is cleared, so no older slot names an extent the image left out. A
    /// dropped collection's slots name released extents, so both pairs are
    /// cleared.
    fn renumber_block(&self, block: &mut [u8]) -> Result<(), ContainerError> {
        let dropped = ContainerCollectionHeader::decode(block)?.dropped;
        let (_, slots) = block.split_at_mut(ContainerCollectionHeader::CHECKPOINT_SLOTS_OFFSET);
        let (checkpoints, generations) = slots.split_at_mut(
            ContainerCollectionHeader::GENERATION_SLOTS_OFFSET
                - ContainerCollectionHeader::CHECKPOINT_SLOTS_OFFSET,
        );
        let checkpoint = ContainerCheckpointSlot::newest(checkpoints).map(|(slot, _)| slot);
        let generation = ContainerGenerationSlot::newest(generations).map(|(slot, _)| slot);
        checkpoints.fill(0);
        generations.fill(0);
        if dropped {
            return Ok(());
        }
        if let Some(slot) = checkpoint {
            let slot = ContainerCheckpointSlot {
                extent: self.index(slot.extent)?,
                locations: self.snapshot(slot.locations)?,
                index: self.snapshot(slot.index)?,
                schemas: self.snapshot(slot.schemas)?,
                dictionary: slot
                    .dictionary
                    .map(|(extent, id, length)| {
                        self.index(extent).map(|extent| (extent, id, length))
                    })
                    .transpose()?,
                ..slot
            };
            checkpoints[..ContainerCheckpointSlot::ENCODED_BYTES].copy_from_slice(&slot.encode());
        }
        if let Some(slot) = generation {
            let slot = ContainerGenerationSlot {
                active_extent: self.index(slot.active_extent)?,
                ..slot
            };
            generations[..ContainerGenerationSlot::ENCODED_BYTES].copy_from_slice(&slot.encode());
        }
        Ok(())
    }

    /// A snapshot reference renumbered. A run's extents are consecutive and
    /// all in use, so they stay consecutive.
    fn snapshot(
        &self,
        snapshot: Option<ContainerSnapshotRef>,
    ) -> Result<Option<ContainerSnapshotRef>, ContainerError> {
        snapshot
            .map(|snapshot| {
                let first = self.index(snapshot.first_extent)?;
                let last = snapshot.first_extent + u64::from(snapshot.extent_count) - 1;
                if self.index(last)? != first + u64::from(snapshot.extent_count) - 1 {
                    return Err(ContainerAllocatorError::InconsistentMap.into());
                }
                Ok(ContainerSnapshotRef {
                    first_extent: first,
                    ..snapshot
                })
            })
            .transpose()
    }
}

/// Writes the blocks of a sealed image in order, then its index, header
/// and superblocks.
struct BlockWriter<'a> {
    file: &'a File,
    cipher: Option<&'a ContainerCipher>,
    /// Where the next stored block goes.
    position: u64,
    blocks: Vec<ContainerSealedBlock>,
}

impl BlockWriter<'_> {
    /// Compress, seal and write the blocks of `bytes`, the next part of the
    /// image, which is a whole number of blocks long.
    fn push(&mut self, bytes: &[u8]) -> Result<(), ContainerError> {
        let block = usize::try_from(ContainerSealedHeader::BLOCK_BYTES)
            .map_err(|_| ContainerFormatError::Oversized)?;
        let chunks: Vec<&[u8]> = bytes.chunks(block).collect();
        let first = self.blocks.len() as u64;
        let threads = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .clamp(1, chunks.len().max(1));
        let per_thread = chunks.len().div_ceil(threads).max(1);
        let cipher = self.cipher;
        let stored: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
            let workers: Vec<_> = chunks
                .chunks(per_thread)
                .enumerate()
                .map(|(worker, part)| {
                    let first = first + (worker * per_thread) as u64;
                    scope.spawn(move || store_blocks(part, first, cipher))
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| {
                    worker.join().unwrap_or_else(|_| {
                        Err(io::Error::other("a block compression thread panicked").into())
                    })
                })
                .collect::<Result<Vec<_>, ContainerError>>()
        })?
        .into_iter()
        .flatten()
        .collect();
        for stored in stored {
            let Some(stored) = stored else {
                self.blocks.push(ContainerSealedBlock::default());
                continue;
            };
            write_all_at(self.file, &stored, self.position)?;
            self.blocks.push(ContainerSealedBlock {
                offset: self.position,
                length: u32::try_from(stored.len()).map_err(|_| ContainerFormatError::Oversized)?,
                checksum: fylo_format::Crc32c::checksum(&stored),
            });
            self.position += stored.len() as u64;
        }
        Ok(())
    }

    /// Write the index after the last block, the header that names it, and
    /// both superblock copies, and flush.
    fn finish(
        self,
        superblock: &ContainerSuperblock,
        image_bytes: u64,
    ) -> Result<ContainerSealedReport, ContainerError> {
        let (index, index_checksum) = ContainerSealedHeader::encode_index(&self.blocks);
        write_all_at(self.file, &index, self.position)?;
        let header = ContainerSealedHeader {
            encrypted: self.cipher.is_some(),
            image_bytes,
            block_count: self.blocks.len() as u64,
            index_offset: self.position,
            index_checksum,
        };
        write_all_at(self.file, &header.encode(), ContainerSealedHeader::OFFSET)?;
        let encoded = superblock.encode();
        write_all_at(self.file, &encoded, 0)?;
        write_all_at(
            self.file,
            &encoded,
            ContainerSuperblock::ENCODED_BYTES as u64,
        )?;
        crate::sync_handle(self.file)?;
        let stored: Vec<&ContainerSealedBlock> = self
            .blocks
            .iter()
            .filter(|block| !block.is_hole())
            .collect();
        Ok(ContainerSealedReport {
            block_bytes: ContainerSealedHeader::BLOCK_BYTES,
            blocks: header.block_count,
            stored_blocks: stored.len() as u64,
            image_bytes,
            stored_bytes: stored.iter().map(|block| u64::from(block.length)).sum(),
        })
    }
}

/// Compress and seal `chunks`, the blocks numbered from `first`: `None` for
/// a block of zeros, which stores nothing.
fn store_blocks(
    chunks: &[&[u8]],
    first: u64,
    cipher: Option<&ContainerCipher>,
) -> Result<Vec<Option<Vec<u8>>>, ContainerError> {
    let mut codec =
        ContainerFrameCodec::with_level(None, ZSTD_SEALED_LEVEL).map_err(codec_failure)?;
    chunks
        .iter()
        .enumerate()
        .map(|(offset, chunk)| {
            if chunk.iter().all(|byte| *byte == 0) {
                return Ok(None);
            }
            let encoded = codec.encode(chunk).map_err(codec_failure)?;
            let mut stored = Vec::with_capacity(1 + encoded.bytes.len());
            stored.push(encoded.codec.code());
            stored.extend_from_slice(&encoded.bytes);
            match cipher {
                Some(cipher) => Ok(Some(cipher.seal(
                    ContainerSealContext::Block {
                        number: first + offset as u64,
                    },
                    &stored,
                )?)),
                None => Ok(Some(stored)),
            }
        })
        .collect()
}

fn codec_failure(error: crate::ContainerCodecError) -> ContainerError {
    match error {
        crate::ContainerCodecError::Library(error) => ContainerError::Io(error),
        other => ContainerError::Io(io::Error::other(other.to_string())),
    }
}

#[cfg(test)]
mod tests;
