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
//! when compression would not shrink it, each with the collection's current
//! dictionary when it has one. Every build reads all three. A
//! codec or flag this build does not know loads as `UnsupportedVersion`,
//! which a scan reports and the segment code refuses
//! ([`super::SegmentReader::frame_error`]) instead of cutting the segment
//! there as a torn tail.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt::Debug;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use fylo_format::{ContainerSuperblock, LogBodyCodec, LogFormatError, LogRecord};

use crate::{
    ContainerCodecError, ContainerDictionary, ContainerFrameCodec, ContainerRecordError,
    ContainerSealContext, ContainerStoredBody, NONCE_BYTES, TAG_BYTES,
};

/// Stored bytes beyond the record: the header byte, a dictionary id, and the
/// nonce and tag a sealed body will carry.
const STORED_OVERHEAD: usize = 1 + 4 + NONCE_BYTES + TAG_BYTES;

/// Where a collection's dictionaries are kept, for one a record names that
/// this process has not loaded: another process trained it since, or this
/// process has only just opened the file.
pub(crate) trait DictionarySource: Debug + Send + Sync {
    /// Dictionary `id`, or `None` when nothing holds it or it cannot be
    /// read. The caller holds no lock of the frames while this runs.
    fn dictionary(&self, id: u32) -> Option<ContainerDictionary>;
}

/// The record bodies of one `.fxdb` collection.
///
/// It keeps one codec per dictionary its records name, zero naming none,
/// created on first use, since each holds zstd contexts that make a small
/// record's encode and decode take microseconds. A dictionary it does not
/// hold is loaded from its source the first time a record names it.
#[derive(Debug)]
pub(crate) struct ContainerFrames {
    collection: u32,
    /// The dictionaries loaded so far, by id.
    dictionaries: Mutex<BTreeMap<u32, ContainerDictionary>>,
    /// Where the others are read from.
    source: Option<Arc<dyn DictionarySource>>,
    /// The dictionary new records are written with; zero for none. It is
    /// always among `dictionaries`, so a write never reads the file.
    writing: AtomicU32,
    /// Write LZ4 natively too, as the browser does.
    lz4: AtomicBool,
    codecs: Mutex<BTreeMap<u32, ContainerFrameCodec>>,
}

impl ContainerFrames {
    /// The bodies of collection `collection`, writing with dictionary
    /// `writing` (zero for none) and reading every one of `dictionaries`.
    pub(crate) fn new(
        collection: u32,
        dictionaries: Vec<ContainerDictionary>,
        writing: u32,
    ) -> Self {
        Self {
            collection,
            dictionaries: Mutex::new(
                dictionaries
                    .into_iter()
                    .map(|dictionary| (dictionary.id, dictionary))
                    .collect(),
            ),
            source: None,
            writing: AtomicU32::new(writing),
            lz4: AtomicBool::new(false),
            codecs: Mutex::new(BTreeMap::new()),
        }
    }

    /// The bodies of collection `collection`, holding no dictionary yet
    /// and loading each from `source` when a record first names it.
    pub(crate) fn with_source(collection: u32, source: Arc<dyn DictionarySource>) -> Self {
        Self {
            source: Some(source),
            ..Self::new(collection, Vec::new(), 0)
        }
    }

    /// Write new records with `dictionary` from now on, or without one.
    pub(crate) fn write_with(&self, dictionary: Option<ContainerDictionary>) {
        let id = dictionary.as_ref().map_or(0, |dictionary| dictionary.id);
        if let Some(dictionary) = dictionary {
            self.dictionaries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(dictionary.id)
                .or_insert(dictionary);
        }
        self.writing.store(id, Ordering::Release);
    }

    /// The dictionary new records are written with, if any.
    pub(crate) fn writing(&self) -> Option<ContainerDictionary> {
        let id = self.writing.load(Ordering::Acquire);
        self.dictionaries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned()
    }

    /// Forget dictionary `id`, once it was released. It is never the one
    /// new records are written with.
    pub(crate) fn forget(&self, id: u32) {
        if id == self.writing.load(Ordering::Acquire) {
            return;
        }
        self.dictionaries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        self.codecs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    /// The superblock's incompatible codec bits for what this build
    /// writes: zstd natively, which compaction writes too, LZ4 in the
    /// browser and wherever a test writes it, and nothing in WASI, which
    /// stores bodies as is.
    pub(crate) fn codec_features(&self) -> u32 {
        const ZSTD: u32 = ContainerSuperblock::INCOMPAT_CODEC_ZSTD;
        const LZ4: u32 = ContainerSuperblock::INCOMPAT_CODEC_LZ4;
        if cfg!(all(target_family = "wasm", not(target_os = "unknown"))) {
            0
        } else if cfg!(target_family = "wasm") {
            LZ4
        } else if self.lz4.load(Ordering::Relaxed) {
            ZSTD | LZ4
        } else {
            ZSTD
        }
    }

    /// Write new records as LZ4, as a browser writer does, so a test can
    /// show native code reading them and compaction rewriting them.
    #[cfg(test)]
    pub(crate) fn write_lz4(&self, lz4: bool) {
        self.lz4.store(lz4, Ordering::Relaxed);
    }

    /// Dictionary `id`, loaded from the source when it is not held yet.
    fn dictionary(&self, id: u32) -> Option<ContainerDictionary> {
        let held = self
            .dictionaries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned();
        if held.is_some() {
            return held;
        }
        let loaded = self.source.as_ref()?.dictionary(id)?;
        Some(
            self.dictionaries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(id)
                .or_insert(loaded)
                .clone(),
        )
    }

    /// Run `body` with the codec of dictionary `id`. No lock is held while
    /// a dictionary is loaded, so a writer holding the file while it
    /// encodes never waits on a reader loading one.
    fn with_codec<T>(
        &self,
        id: u32,
        body: impl FnOnce(&mut ContainerFrameCodec) -> Result<T, ContainerRecordError>,
    ) -> Result<T, ContainerRecordError> {
        let mut codecs = self.codecs.lock().unwrap_or_else(PoisonError::into_inner);
        if !codecs.contains_key(&id) {
            drop(codecs);
            let dictionary = match id {
                0 => None,
                id => Some(
                    self.dictionary(id)
                        .ok_or(ContainerCodecError::WrongDictionary)?,
                ),
            };
            let codec = ContainerFrameCodec::new(dictionary)?;
            codecs = self.codecs.lock().unwrap_or_else(PoisonError::into_inner);
            if let Entry::Vacant(slot) = codecs.entry(id) {
                slot.insert(codec);
            }
        }
        let codec = codecs
            .get_mut(&id)
            .ok_or(ContainerCodecError::WrongDictionary)?;
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
    /// since records are not sealed yet. A dictionary that is missing or
    /// that zstd refuses is `MissingDictionary`, which no scan takes for a
    /// torn tail either.
    fn format_error(error: &ContainerRecordError) -> LogFormatError {
        match error {
            ContainerRecordError::Unsupported
            | ContainerRecordError::Locked
            | ContainerRecordError::Codec(ContainerCodecError::Unsupported) => {
                LogFormatError::UnsupportedVersion
            }
            ContainerRecordError::Codec(
                ContainerCodecError::WrongDictionary | ContainerCodecError::BadDictionary,
            ) => LogFormatError::MissingDictionary,
            ContainerRecordError::Codec(ContainerCodecError::Oversized) => {
                LogFormatError::Oversized
            }
            _ => LogFormatError::InvalidField,
        }
    }
}

impl LogBodyCodec for ContainerFrames {
    fn store(&self, body: Vec<u8>) -> Result<Vec<u8>, LogFormatError> {
        self.with_codec(self.writing.load(Ordering::Acquire), |codec| {
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
