//! Reading a sealed `.vdbx` file (ADR 0016 sections 8 and 11).
//!
//! A sealed file is a read-only image of a container, cut into 64 KiB
//! blocks that are compressed one at a time at zstd level 19 and, in an
//! encrypted file, sealed with the block number as associated data, so a
//! block copied to another place does not open. The superblock copies stay
//! in the clear at the start of the file, and the block index after the
//! last block says where each block is ([`ContainerSealedHeader`]). Every
//! read of the container goes through [`SealedImage::read`], so a point
//! read decompresses the one block that holds its record, and the code
//! above, the segment store's included, reads the image as it reads any
//! container.
//!
//! The records inside are stored as is, and the image itself is plain: in
//! an encrypted sealed file the blocks are what is sealed, so the frames,
//! directory, dictionaries and snapshots inside carry no seal of their own.
//!
//! Blocks come from disk and are not trusted: each is checked against its
//! CRC-32C in the index before it is opened, and must decompress to exactly
//! the bytes of the image it covers. A few decoded blocks are kept, since a
//! scan reads a block's records one after another.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use fylo_format::{
    ContainerCodec, ContainerFormatError, ContainerSealedBlock, ContainerSealedHeader,
    ContainerSuperblock,
};
use fylo_vfs::File;

use super::{ContainerError, read_exact_at};
use crate::{ContainerCipher, ContainerFrameCodec, ContainerSealContext};

/// Decoded blocks kept for the next read.
const CACHED_BLOCKS: usize = 16;

/// The block layout of a sealed file, and the key that opens its blocks.
pub(super) struct SealedImage {
    header: ContainerSealedHeader,
    blocks: Vec<ContainerSealedBlock>,
    cipher: Option<Arc<ContainerCipher>>,
    codec: Mutex<ContainerFrameCodec>,
    /// The blocks decoded last, by number, the newest last.
    cache: Mutex<Vec<(u64, Arc<[u8]>)>>,
}

impl std::fmt::Debug for SealedImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealedImage")
            .field("header", &self.header)
            .field("locked", &self.is_locked())
            .finish_non_exhaustive()
    }
}

/// What `fylo inspect` reports about a sealed file's blocks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSealedReport {
    /// Bytes of the image one block holds.
    pub block_bytes: u64,
    /// Blocks in the image.
    pub blocks: u64,
    /// Blocks that store bytes; the others are all zeros and store none.
    pub stored_blocks: u64,
    /// Bytes of the image: the container the blocks hold, uncompressed.
    pub image_bytes: u64,
    /// Bytes the blocks store.
    pub stored_bytes: u64,
}

impl SealedImage {
    /// Read the block layout of the sealed file `file`, whose superblock is
    /// `superblock`.
    ///
    /// # Errors
    ///
    /// Returns a decoding error for a damaged header or index, or one that
    /// does not describe the container the superblock does or fit in the
    /// file, or the read failure.
    pub(super) fn load(
        file: &File,
        superblock: &ContainerSuperblock,
    ) -> Result<Self, ContainerError> {
        let mut bytes = [0_u8; ContainerSealedHeader::ENCODED_BYTES];
        read_exact_at(file, &mut bytes, ContainerSealedHeader::OFFSET)?;
        let header = ContainerSealedHeader::decode(&bytes)?;
        let image = superblock
            .extent_count
            .checked_mul(superblock.extent_bytes());
        let index_end = header.index_offset + header.index_bytes();
        if image != Some(header.image_bytes)
            || header.encrypted != superblock.is_encrypted()
            || index_end > file.metadata()?.len()
        {
            return Err(ContainerFormatError::InvalidField.into());
        }
        let length =
            usize::try_from(header.index_bytes()).map_err(|_| ContainerFormatError::Oversized)?;
        let mut index = vec![0_u8; length];
        read_exact_at(file, &mut index, header.index_offset)?;
        let blocks = header.decode_index(&index)?;
        let codec =
            ContainerFrameCodec::new(None).map_err(|_| ContainerFormatError::InvalidField)?;
        Ok(Self {
            header,
            blocks,
            cipher: None,
            codec: Mutex::new(codec),
            cache: Mutex::new(Vec::new()),
        })
    }

    /// Bytes of the image.
    pub(super) const fn image_bytes(&self) -> u64 {
        self.header.image_bytes
    }

    /// Whether the blocks are sealed and no key has opened them yet.
    pub(super) const fn is_locked(&self) -> bool {
        self.header.encrypted && self.cipher.is_none()
    }

    /// Open the blocks with `cipher` from now on, or lock them again with
    /// `None`.
    pub(super) fn set_cipher(&mut self, cipher: Option<Arc<ContainerCipher>>) {
        self.cipher = cipher;
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Read `buffer.len()` bytes of the image at `offset`. The superblock
    /// region is read from the file itself, the rest from the blocks.
    ///
    /// # Errors
    ///
    /// Returns `UnexpectedEof` past the image's end, `PermissionDenied`
    /// for a locked file, `InvalidData` for a damaged block, or the read
    /// failure.
    pub(super) fn read(&self, file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .filter(|end| *end <= self.header.image_bytes)
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let mut done = 0;
        if offset < ContainerSealedHeader::OFFSET {
            let take = usize::try_from(end.min(ContainerSealedHeader::OFFSET) - offset)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            read_exact_at(file, &mut buffer[..take], offset)?;
            done = take;
        }
        while done < buffer.len() {
            let at = offset + done as u64;
            let number = at / ContainerSealedHeader::BLOCK_BYTES;
            let within = usize::try_from(at % ContainerSealedHeader::BLOCK_BYTES)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            let block = self.block(file, number)?;
            let take = (block.len() - within).min(buffer.len() - done);
            buffer[done..done + take].copy_from_slice(&block[within..within + take]);
            done += take;
        }
        Ok(())
    }

    /// Block `number`, decoded, from the cache or the file.
    fn block(&self, file: &File, number: u64) -> io::Result<Arc<[u8]>> {
        {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(position) = cache.iter().position(|(cached, _)| *cached == number) {
                let entry = cache.remove(position);
                let block = Arc::clone(&entry.1);
                cache.push(entry);
                return Ok(block);
            }
        }
        let block: Arc<[u8]> = self
            .decode(file, number)
            .map_err(|damage| match damage {
                BlockDamage::Io(error) => error,
                BlockDamage::Locked => io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the sealed .vdbx file is encrypted and needs its key",
                ),
                BlockDamage::Damaged(message) => io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("sealed block {number} is damaged: {message}"),
                ),
            })?
            .into();
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() >= CACHED_BLOCKS {
            cache.remove(0);
        }
        cache.push((number, Arc::clone(&block)));
        Ok(block)
    }

    /// Bytes of the image block `number` covers.
    fn block_length(&self, number: u64) -> usize {
        let start = number * ContainerSealedHeader::BLOCK_BYTES;
        let length = (self.header.image_bytes - start).min(ContainerSealedHeader::BLOCK_BYTES);
        usize::try_from(length).unwrap_or(0)
    }

    /// Read, check, open and decompress block `number`.
    fn decode(&self, file: &File, number: u64) -> Result<Vec<u8>, BlockDamage> {
        let stored = self.stored(file, number)?;
        let length = self.block_length(number);
        let Some(stored) = stored else {
            return Ok(vec![0; length]);
        };
        let opened = match (&self.cipher, self.header.encrypted) {
            (_, false) => stored,
            (Some(cipher), true) => cipher
                .open(ContainerSealContext::Block { number }, &stored)
                .map_err(|error| BlockDamage::Damaged(error.to_string()))?,
            (None, true) => return Err(BlockDamage::Locked),
        };
        let (codec, payload) = opened
            .split_first()
            .ok_or_else(|| BlockDamage::Damaged("it stores no codec".into()))?;
        let codec = ContainerCodec::from_code(*codec)
            .ok()
            .filter(|codec| *codec != ContainerCodec::Lz4)
            .ok_or_else(|| BlockDamage::Damaged("it names an unknown codec".into()))?;
        let decoded = self
            .codec
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .decode(codec, 0, payload, length)
            .map_err(|error| BlockDamage::Damaged(error.to_string()))?;
        if decoded.len() != length {
            return Err(BlockDamage::Damaged(format!(
                "it holds {} bytes, not {length}",
                decoded.len()
            )));
        }
        Ok(decoded)
    }

    /// Block `number`'s stored bytes checked against the index, or `None`
    /// for a hole.
    fn stored(&self, file: &File, number: u64) -> Result<Option<Vec<u8>>, BlockDamage> {
        let entry = usize::try_from(number)
            .ok()
            .and_then(|number| self.blocks.get(number))
            .ok_or_else(|| BlockDamage::Io(io::ErrorKind::UnexpectedEof.into()))?;
        if entry.is_hole() {
            return Ok(None);
        }
        let mut stored = vec![0_u8; entry.length as usize];
        read_exact_at(file, &mut stored, entry.offset).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                BlockDamage::Damaged("the file ends inside it".into())
            } else {
                BlockDamage::Io(error)
            }
        })?;
        entry
            .check(&stored)
            .map_err(|error| BlockDamage::Damaged(error.to_string()))?;
        Ok(Some(stored))
    }

    /// Check every block, for `fylo verify`: its stored bytes against the
    /// index's CRC-32C, which needs no key, and, unless the file is locked,
    /// that it opens and decompresses to its share of the image. Returns
    /// the damaged blocks with what is wrong with each.
    ///
    /// # Errors
    ///
    /// Returns a read failure other than the file ending early.
    pub(super) fn damaged_blocks(&self, file: &File) -> io::Result<Vec<(u64, String)>> {
        let mut damaged = Vec::new();
        for number in 0..self.header.block_count {
            let checked = if self.is_locked() {
                self.stored(file, number).map(|_| ())
            } else {
                self.decode(file, number).map(|_| ())
            };
            match checked {
                Ok(()) | Err(BlockDamage::Locked) => {}
                Err(BlockDamage::Io(error)) => return Err(error),
                Err(BlockDamage::Damaged(message)) => damaged.push((number, message)),
            }
        }
        Ok(damaged)
    }

    /// The blocks, for `fylo inspect`.
    pub(super) fn report(&self) -> ContainerSealedReport {
        let stored: Vec<&ContainerSealedBlock> = self
            .blocks
            .iter()
            .filter(|block| !block.is_hole())
            .collect();
        ContainerSealedReport {
            block_bytes: ContainerSealedHeader::BLOCK_BYTES,
            blocks: self.header.block_count,
            stored_blocks: stored.len() as u64,
            image_bytes: self.header.image_bytes,
            stored_bytes: stored.iter().map(|block| u64::from(block.length)).sum(),
        }
    }
}

/// Why a block could not be decoded.
enum BlockDamage {
    /// The file could not be read.
    Io(io::Error),
    /// The block is sealed and the file is locked.
    Locked,
    /// The block is damaged: what is wrong with it.
    Damaged(String),
}
