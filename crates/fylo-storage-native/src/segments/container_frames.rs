//! How a `.fxdb` collection stores record bodies in their frames
//! (ADR 0016 sections 4 and 8, `docs/fxdb-segment-backend.md`, "Encryption
//! and codecs").
//!
//! The frame is a directory segment's: sync marker, length, CRC-32C, body.
//! The body is a [`ContainerStoredBody`]: the codec header byte, the
//! dictionary id when one was used, then the record compressed on its own.
//! The length and checksum cover those stored bytes. A directory collection
//! never comes here, so its frames stay byte for byte what they were.
//!
//! Natively a record is written as zstd, in the browser as LZ4, and as is
//! when compression would not shrink it. Every build reads all three. A
//! codec or flag this build does not know loads as `UnsupportedVersion`,
//! which a scan reports and the segment code refuses
//! ([`super::SegmentReader::frame_error`]) instead of cutting the segment
//! there as a torn tail.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use fylo_format::{LogBodyCodec, LogFormatError, LogRecord};

use crate::{
    ContainerCodecError, ContainerDictionary, ContainerFrameCodec, ContainerRecordError,
    ContainerSealContext, ContainerStoredBody, NONCE_BYTES, TAG_BYTES,
};

/// Stored bytes beyond the record: the header byte, a dictionary id, and the
/// nonce and tag a sealed body will carry.
const STORED_OVERHEAD: usize = 1 + 4 + NONCE_BYTES + TAG_BYTES;

/// The record bodies of one `.fxdb` collection.
///
/// It keeps one codec per dictionary its records name, zero naming none,
/// created on first use, since each holds zstd contexts that make a small
/// record's encode and decode take microseconds.
#[derive(Debug)]
pub(crate) struct ContainerFrames {
    collection: u32,
    /// The dictionaries records may name, by id.
    dictionaries: BTreeMap<u32, ContainerDictionary>,
    /// The dictionary new records are written with; zero for none.
    writing: u32,
    /// Write LZ4 natively too, as the browser does.
    lz4: AtomicBool,
    codecs: Mutex<BTreeMap<u32, ContainerFrameCodec>>,
}

impl ContainerFrames {
    /// The bodies of collection `collection`, writing with dictionary
    /// `writing` (zero for none) and reading every one of `dictionaries`.
    ///
    /// Dictionaries are not trained or stored yet, so a collection has none
    /// and a record that names one is refused as `WrongDictionary`.
    pub(crate) fn new(
        collection: u32,
        dictionaries: Vec<ContainerDictionary>,
        writing: u32,
    ) -> Self {
        Self {
            collection,
            dictionaries: dictionaries
                .into_iter()
                .map(|dictionary| (dictionary.id, dictionary))
                .collect(),
            writing,
            lz4: AtomicBool::new(false),
            codecs: Mutex::new(BTreeMap::new()),
        }
    }

    /// Write new records as LZ4, as a browser writer does, so a test can
    /// show native code reading them and compaction rewriting them.
    #[cfg(test)]
    pub(crate) fn write_lz4(&self, lz4: bool) {
        self.lz4.store(lz4, Ordering::Relaxed);
    }

    /// Run `body` with the codec of dictionary `id`.
    fn with_codec<T>(
        &self,
        id: u32,
        body: impl FnOnce(&mut ContainerFrameCodec) -> Result<T, ContainerRecordError>,
    ) -> Result<T, ContainerRecordError> {
        let mut codecs = self.codecs.lock().unwrap_or_else(PoisonError::into_inner);
        let codec = match codecs.entry(id) {
            Entry::Occupied(codec) => codec.into_mut(),
            Entry::Vacant(slot) => {
                let dictionary = match id {
                    0 => None,
                    id => Some(
                        self.dictionaries
                            .get(&id)
                            .cloned()
                            .ok_or(ContainerCodecError::WrongDictionary)?,
                    ),
                };
                slot.insert(ContainerFrameCodec::new(dictionary)?)
            }
        };
        body(codec)
    }

    /// Nothing is sealed yet: an encrypted file's records wait for their
    /// frame binding (ADR 0016 section 9), so the context is never used.
    fn context(&self) -> ContainerSealContext {
        ContainerSealContext::Record {
            collection: self.collection,
            lsn: 0,
            entry: 0,
        }
    }

    /// A stored body's failure as a scan sees it. What this build cannot
    /// read is `UnsupportedVersion`, never damage: a sealed body included,
    /// since records are not sealed yet.
    fn format_error(error: &ContainerRecordError) -> LogFormatError {
        match error {
            ContainerRecordError::Unsupported
            | ContainerRecordError::Locked
            | ContainerRecordError::Codec(ContainerCodecError::Unsupported) => {
                LogFormatError::UnsupportedVersion
            }
            ContainerRecordError::Codec(ContainerCodecError::Oversized) => {
                LogFormatError::Oversized
            }
            _ => LogFormatError::InvalidField,
        }
    }
}

impl LogBodyCodec for ContainerFrames {
    fn store(&self, body: Vec<u8>) -> Result<Vec<u8>, LogFormatError> {
        self.with_codec(self.writing, |codec| {
            let lz4 = self.lz4.load(Ordering::Relaxed);
            ContainerStoredBody::encode(codec, None, self.context(), &body, lz4)
        })
        .map_err(|error| Self::format_error(&error))
    }

    fn load<'a>(&self, stored: &'a [u8]) -> Result<Cow<'a, [u8]>, LogFormatError> {
        let parsed =
            ContainerStoredBody::parse(stored).map_err(|error| Self::format_error(&error))?;
        if let Some(body) = parsed.plain() {
            return Ok(Cow::Borrowed(body));
        }
        self.with_codec(parsed.dictionary_id, |codec| {
            parsed.decode(codec, None, self.context(), LogRecord::MAX_BODY_BYTES)
        })
        .map(Cow::Owned)
        .map_err(|error| Self::format_error(&error))
    }

    fn stored_bytes(&self) -> RangeInclusive<usize> {
        1..=LogRecord::MAX_BODY_BYTES + STORED_OVERHEAD
    }

    /// The stored bytes the length names, or all that follow when it
    /// reaches past them, decompressed if they still decompress.
    fn unchecked_body<'a>(&self, frame: &'a [u8]) -> Option<Cow<'a, [u8]>> {
        let length = frame
            .first_chunk::<4>()
            .map(|length| u32::from_le_bytes(*length))?;
        let stored = frame.get(LogRecord::FRAME_BYTES..)?;
        let stored = usize::try_from(length)
            .ok()
            .and_then(|length| stored.get(..length))
            .unwrap_or(stored);
        self.load(stored).ok()
    }
}

#[cfg(test)]
mod tests;
