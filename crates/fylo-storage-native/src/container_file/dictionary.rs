//! Compression dictionaries in a `.vdbx` container (ADR 0016 sections 3
//! and 8).
//!
//! A dictionary lives in one `dictionary` extent its collection owns: a
//! [`ContainerDictionaryHeader`] naming its id, then the zstd dictionary,
//! sealed in an encrypted file for its collection and id. Nothing in it
//! depends on a checkpoint, so a checkpoint slot that keeps naming it needs
//! no copy, unlike a snapshot run.
//!
//! A dictionary is part of the source of truth: the records compressed with
//! it cannot be read without it. The checkpoint slot names the one new
//! records are written with; older ones are found by their headers among the
//! collection's `dictionary` extents, and are released only once no record
//! names them.

use fylo_format::{ContainerDictionaryHeader, ContainerExtentKind, ContainerFormatError};

use super::{ContainerError, ContainerFile};
use crate::{ContainerDictionary, ContainerSealContext};

const DICTIONARY: ContainerExtentKind = ContainerExtentKind::Dictionary;
const HEADER_BYTES: usize = ContainerDictionaryHeader::ENCODED_BYTES;

impl ContainerFile {
    /// Write `dictionary` of collection `collection` into a fresh
    /// `dictionary` extent, seal the extent in the map, and flush. Returns
    /// what the checkpoint slot records: the extent, the id and the stored
    /// length.
    ///
    /// The caller checkpoints the map before any slot names the extent, and
    /// writes no record with the dictionary before a slot does, so a stop
    /// in between leaves a dictionary no record names.
    ///
    /// # Errors
    ///
    /// Returns `ReadOnly` or `Locked`, `InvalidField` for a zero id,
    /// `Oversized` for a dictionary the format cannot hold, `Cipher(Random)`
    /// when no nonce can be drawn, or the write, resize or flush failure.
    /// On a failure the extent is released.
    pub fn write_dictionary(
        &mut self,
        collection: u32,
        dictionary: &ContainerDictionary,
    ) -> Result<(u64, u32, u32), ContainerError> {
        self.require_writable()?;
        let context = ContainerSealContext::Dictionary {
            collection,
            id: dictionary.id,
        };
        let stored = match self.cipher() {
            Some(cipher) => cipher.seal(context, &dictionary.bytes)?,
            None => dictionary.bytes.clone(),
        };
        let header =
            ContainerDictionaryHeader::for_stored(dictionary.id, self.cipher().is_some(), &stored)?;
        if (HEADER_BYTES + stored.len()) as u64 > self.payload_capacity() {
            return Err(ContainerFormatError::Oversized.into());
        }
        let allocation = self.allocate(DICTIONARY, collection)?;
        let written = self
            .write_payload(allocation.index, 0, &header.encode())
            .and_then(|()| self.write_payload(allocation.index, HEADER_BYTES as u64, &stored))
            .and_then(|()| self.sync());
        if let Err(error) = written {
            self.allocator.release(allocation.index)?;
            return Err(error.into());
        }
        // A dictionary never changes; its live bytes are what it stores.
        self.allocator
            .seal(allocation.index, (HEADER_BYTES + stored.len()) as u64)?;
        Ok((allocation.index, dictionary.id, header.length))
    }

    /// The header of the dictionary in `extent`, after checking the
    /// extent's own header names a dictionary of `collection` at its index.
    ///
    /// # Errors
    ///
    /// Returns `InvalidField` for an extent that holds something else, a
    /// header decoding error, or the read failure.
    pub fn dictionary_header(
        &self,
        collection: u32,
        extent: u64,
    ) -> Result<ContainerDictionaryHeader, ContainerError> {
        let header = self.read_extent_header_at(extent)?;
        if (header.index, header.kind, header.owner) != (extent, DICTIONARY, collection) {
            return Err(ContainerFormatError::InvalidField.into());
        }
        let mut bytes = [0_u8; HEADER_BYTES];
        self.read_payload(extent, 0, &mut bytes)?;
        Ok(ContainerDictionaryHeader::decode(&bytes)?)
    }

    /// Read the dictionary of `collection` in `extent`, checking its
    /// header and stored bytes, and opening it with the key of an encrypted
    /// file. Works on readers.
    ///
    /// # Errors
    ///
    /// Returns what [`Self::dictionary_header`] does, `ChecksumMismatch`
    /// for stored bytes that do not match their header, `Locked` for a
    /// locked encrypted file, `Cipher(Unauthenticated)` for a dictionary
    /// sealed for another place, or `InvalidField` for a sealed dictionary
    /// in a plain file or a plain one in an encrypted file.
    pub fn read_dictionary(
        &self,
        collection: u32,
        extent: u64,
    ) -> Result<ContainerDictionary, ContainerError> {
        let header = self.dictionary_header(collection, extent)?;
        if self.is_locked() {
            return Err(ContainerError::Locked);
        }
        if header.sealed != self.seals_units() {
            return Err(ContainerFormatError::InvalidField.into());
        }
        let mut stored = vec![0_u8; header.length as usize];
        self.read_payload(extent, HEADER_BYTES as u64, &mut stored)?;
        header.check(&stored)?;
        let bytes = match self.cipher() {
            Some(cipher) => cipher.open(
                ContainerSealContext::Dictionary {
                    collection,
                    id: header.id,
                },
                &stored,
            )?,
            None => stored,
        };
        Ok(ContainerDictionary {
            id: header.id,
            bytes,
        })
    }

    /// The dictionaries of `collection` the map shows, as (extent, id), in
    /// extent order. An extent whose header does not decode has no id it
    /// can be found by, and is left out.
    ///
    /// # Errors
    ///
    /// Returns a read failure.
    pub fn dictionary_extents(&self, collection: u32) -> Result<Vec<(u64, u32)>, ContainerError> {
        let mut found = Vec::new();
        for (extent, _) in self.allocator.owned(DICTIONARY, collection) {
            match self.dictionary_header(collection, extent) {
                Ok(header) => found.push((extent, header.id)),
                Err(ContainerError::Io(error))
                    if error.kind() != std::io::ErrorKind::UnexpectedEof =>
                {
                    return Err(ContainerError::Io(error));
                }
                Err(_) => {}
            }
        }
        Ok(found)
    }

    /// Find and read dictionary `id` of `collection` among the extents the
    /// map shows, or `None` when no extent holds it.
    ///
    /// # Errors
    ///
    /// Returns the failure to read the extent that holds it, or a read
    /// failure while looking.
    pub fn find_dictionary(
        &self,
        collection: u32,
        id: u32,
    ) -> Result<Option<ContainerDictionary>, ContainerError> {
        let Some((extent, _)) = self
            .dictionary_extents(collection)?
            .into_iter()
            .find(|(_, found)| *found == id)
        else {
            return Ok(None);
        };
        self.read_dictionary(collection, extent).map(Some)
    }

    /// Release a dictionary extent no record names any longer. It is
    /// reused only after the next [`Self::checkpoint`].
    ///
    /// # Errors
    ///
    /// Returns `ReadOnly` or `Locked`, `InvalidField` for an extent that
    /// is not a dictionary of `collection`, or `Allocator(NotInUse)`.
    pub fn release_dictionary(
        &mut self,
        collection: u32,
        extent: u64,
    ) -> Result<(), ContainerError> {
        self.require_writable()?;
        let header = self.read_extent_header_at(extent)?;
        if (header.index, header.kind, header.owner) != (extent, DICTIONARY, collection) {
            return Err(ContainerFormatError::InvalidField.into());
        }
        Ok(self.allocator.release(extent)?)
    }
}

#[cfg(test)]
mod tests;
