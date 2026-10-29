//! A container collection's compression dictionaries (ADR 0016 section 8,
//! `docs/fxdb-segment-backend.md`, "Dictionaries").
//!
//! A collection writes without a dictionary until its records reach 1 MiB.
//! The first checkpoint that finds them trains a 32 KiB zstd dictionary on
//! those first records, writes it to a `dictionary` extent and names it in
//! the checkpoint slot; from then on its records are written with it. A
//! compaction trains another on the newest records, and keeps it only when
//! it stores them at least 10% smaller; the copy and every later record use
//! it. Each record keeps the dictionary it was written with until a
//! compaction rewrites it, so every dictionary a record names stays in the
//! file, and a compaction releases those no record names any longer.
//!
//! Only native writers train: the zstd trainer is the C library's. A
//! browser writer writes LZ4 with the dictionary the slot names, and every
//! build reads with whichever dictionary a record names, loading it from
//! the file the first time.

use std::collections::BTreeSet;
use std::sync::Arc;

use fylo_format::{ContainerCheckpointSlot, LogRecord, LogRecordKind, LogScan, LogSegmentHeader};

use super::{ContainerMedium, ContainerOpen, INDEX, LOCATIONS, Open, SCHEMAS, Shared};
use crate::segments::{DictionarySource, SegmentMedium};
use crate::{
    ContainerDictionary, ContainerFrameCodec, ContainerStoredBody, DICTIONARY_TRAINING_BYTES,
    NativeStorageError,
};

/// The share of the current dictionary's stored bytes a retrained one must
/// stay within to replace it: 90%, so at least 10% smaller.
const RETRAIN_PERCENT: usize = 90;

/// A collection's dictionaries as the file holds them, for the frames to
/// load one a record names.
#[derive(Debug)]
pub(super) struct FileDictionaries {
    pub(super) file: Arc<ContainerOpen>,
    pub(super) collection: u32,
}

impl DictionarySource for FileDictionaries {
    /// The map this open holds, and when that has no such dictionary, the
    /// map another process checkpointed since.
    fn dictionary(&self, id: u32) -> Option<ContainerDictionary> {
        let mut open = self.file.lock();
        if let Ok(Some(dictionary)) = open.find_dictionary(self.collection, id) {
            return Some(dictionary);
        }
        if !open.refresh().ok()? {
            return None;
        }
        open.find_dictionary(self.collection, id).ok().flatten()
    }
}

impl Shared {
    /// The dictionary the newest checkpoint `slot` names, read from `open`,
    /// or `None` when it names none or it cannot be read; records are then
    /// written without one.
    pub(super) fn named_dictionary(
        &self,
        open: &Open,
        slot: Option<&ContainerCheckpointSlot>,
    ) -> Option<ContainerDictionary> {
        let (extent, id, _) = slot?.dictionary?;
        if let Some(writing) = self.frames.writing().filter(|held| held.id == id) {
            return Some(writing);
        }
        open.read_dictionary(self.collection, extent)
            .ok()
            .filter(|dictionary| dictionary.id == id)
    }
}

impl ContainerMedium {
    /// The dictionary a checkpoint after `previous` trains: one, when the
    /// collection has none yet and its records reach 1 MiB. Its id is set
    /// when it is written.
    pub(super) fn dictionary_due(
        &self,
        previous: Option<&ContainerCheckpointSlot>,
    ) -> Result<Option<ContainerDictionary>, NativeStorageError> {
        if !cfg!(not(target_family = "wasm"))
            || previous.is_some_and(|slot| slot.dictionary.is_some())
            || self.shared.state().untrainable
        {
            return Ok(None);
        }
        let samples = self.samples(false)?;
        if samples.iter().map(Vec::len).sum::<usize>() < DICTIONARY_TRAINING_BYTES {
            return Ok(None);
        }
        Ok(self.trained(&samples))
    }

    /// Before a compaction: train a dictionary on the newest 1 MiB of
    /// records and make it current when it stores them at least 10%
    /// smaller than the current one does, or than no dictionary does.
    /// Returns whether it did.
    pub(super) fn retrain(&self) -> Result<bool, NativeStorageError> {
        if !cfg!(not(target_family = "wasm")) || self.shared.state().untrainable {
            return Ok(false);
        }
        let samples = self.samples(true)?;
        if samples.iter().map(Vec::len).sum::<usize>() < DICTIONARY_TRAINING_BYTES {
            return Ok(false);
        }
        let Some(candidate) = self.trained(&samples) else {
            return Ok(false);
        };
        let current = self.newest_checkpoint()?.and_then(|slot| {
            self.shared
                .named_dictionary(&self.shared.file(), Some(&slot))
        });
        let (Some(now), Some(then)) = (
            stored_bytes(current.as_ref(), &samples),
            stored_bytes(Some(&candidate), &samples),
        ) else {
            return Ok(false);
        };
        if then * 100 > now * RETRAIN_PERCENT {
            return Ok(false);
        }
        self.replace_dictionary(candidate)?;
        Ok(true)
    }

    /// A dictionary trained on `samples`, or `None` when zstd cannot train
    /// one from them, which this process then stops asking for.
    fn trained(&self, samples: &[Vec<u8>]) -> Option<ContainerDictionary> {
        #[cfg(not(target_family = "wasm"))]
        {
            let trained = crate::train_dictionary(u32::MAX, samples);
            if trained.is_none() {
                self.shared.state().untrainable = true;
            }
            trained
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = samples;
            None
        }
    }

    /// Record bodies of the collection's commits that carry documents, as
    /// the frames compress them, up to [`DICTIONARY_TRAINING_BYTES`]: the
    /// first ones, or with `newest` the newest.
    pub(super) fn samples(&self, newest: bool) -> Result<Vec<Vec<u8>>, NativeStorageError> {
        let mut numbers = self.segment_numbers()?;
        if newest {
            numbers.reverse();
        }
        let frames = self.frame_bodies();
        let (mut samples, mut total) = (Vec::new(), 0);
        for number in numbers {
            let Some(bytes) = self.read_segment(number)? else {
                continue;
            };
            let Ok(scan) = LogScan::segment_with(&bytes, &*frames) else {
                continue;
            };
            let mut records = scan.records;
            if newest {
                records.reverse();
            }
            for body in records.iter().filter_map(|(_, record)| sample(record)) {
                total += body.len();
                samples.push(body);
                if total >= DICTIONARY_TRAINING_BYTES {
                    return Ok(samples);
                }
            }
        }
        Ok(samples)
    }

    /// Before records are imported into a collection that has no
    /// dictionary: train one on `records`, the first to come, when their
    /// bodies reach 1 MiB, and make it current, so that the import writes
    /// them with it as a checkpoint would have the records after it.
    pub(super) fn prime(&self, records: &[LogRecord]) -> Result<bool, NativeStorageError> {
        if !cfg!(not(target_family = "wasm"))
            || self.shared.state().untrainable
            || self
                .newest_checkpoint()?
                .is_some_and(|slot| slot.dictionary.is_some())
        {
            return Ok(false);
        }
        let samples: Vec<Vec<u8>> = records.iter().filter_map(sample).collect();
        if samples.iter().map(Vec::len).sum::<usize>() < DICTIONARY_TRAINING_BYTES {
            return Ok(false);
        }
        let Some(dictionary) = self.trained(&samples) else {
            return Ok(false);
        };
        self.replace_dictionary(dictionary)?;
        Ok(true)
    }

    /// Make `dictionary` current: write it to a `dictionary` extent,
    /// checkpoint the map so it shows the extent, and record a checkpoint
    /// slot that names it and keeps the newest slot's position and runs.
    /// Records are written with it only once the slot is recorded, so a
    /// stop before leaves a dictionary no record names, which the next
    /// compaction releases.
    fn replace_dictionary(
        &self,
        mut dictionary: ContainerDictionary,
    ) -> Result<(), NativeStorageError> {
        let (id, block) = (self.shared.collection, self.shared.block);
        let dictionary = self.shared.writable()?.change_map(|file| {
            let previous = file.read_checkpoint(block).map_err(Self::error)?;
            dictionary.id = Self::next_dictionary_id(file, id, previous.as_ref())?;
            let named = file
                .write_dictionary(id, &dictionary)
                .map_err(Self::error)?;
            let checkpoint = previous.map_or(1, |previous| previous.checkpoint + 1);
            let slot = match previous {
                Some(previous) => ContainerCheckpointSlot {
                    checkpoint,
                    locations: previous
                        .locations
                        .map(|run| {
                            Self::carried(
                                file,
                                (id, LOCATIONS),
                                (previous.checkpoint, run),
                                checkpoint,
                            )
                        })
                        .transpose()
                        .map_err(Self::error)?,
                    index: Self::carried_if_readable(
                        file,
                        (id, INDEX),
                        (previous.checkpoint, previous.index),
                        checkpoint,
                    )
                    .map_err(Self::error)?,
                    schemas: Self::carried_schemas(file, id, Some(&previous), checkpoint)
                        .map_err(Self::error)?,
                    dictionary: Some(named),
                    ..previous
                },
                None => ContainerCheckpointSlot {
                    checkpoint,
                    lsn: 0,
                    extent: 0,
                    extent_sequence: 0,
                    offset: 0,
                    locations: None,
                    index: None,
                    schemas: None,
                    dictionary: Some(named),
                },
            };
            file.checkpoint()?;
            file.record_checkpoint(block, slot).map_err(Self::error)?;
            for (kind, kept) in [
                (LOCATIONS, slot.locations),
                (INDEX, slot.index),
                (SCHEMAS, slot.schemas),
            ] {
                Self::release_runs(file, (id, kind), kept.as_ref()).map_err(Self::error)?;
            }
            Ok(dictionary)
        })?;
        Self::forget_index(&self.identity());
        self.shared.frames.write_with(Some(dictionary));
        Ok(())
    }

    /// The id for a new dictionary of collection `id`: above every one it
    /// holds and the one its newest checkpoint `previous` names.
    pub(super) fn next_dictionary_id(
        file: &Open,
        id: u32,
        previous: Option<&ContainerCheckpointSlot>,
    ) -> Result<u32, NativeStorageError> {
        let highest = file
            .dictionary_extents(id)
            .map_err(Self::error)?
            .into_iter()
            .map(|(_, id)| id)
            .chain(
                previous
                    .and_then(|slot| slot.dictionary)
                    .map(|(_, id, _)| id),
            )
            .max()
            .unwrap_or(0);
        Ok(highest.saturating_add(1).max(1))
    }

    /// After a compaction: release every dictionary of the collection that
    /// neither the newest checkpoint nor any record names, and a second
    /// extent holding a dictionary another holds too, as a shrink that
    /// stopped while moving one leaves. Nothing is released when a segment
    /// cannot be read to its end, since a record past the damage might name
    /// one.
    pub(super) fn release_unused(&self) -> Result<(), NativeStorageError> {
        let Some(named) = self.named_by_records()? else {
            return Ok(());
        };
        let (id, block) = (self.shared.collection, self.shared.block);
        let released = self.shared.writable()?.change_map(|file| {
            let current = file
                .read_checkpoint(block)
                .map_err(Self::error)?
                .and_then(|slot| slot.dictionary);
            let mut extents = file.dictionary_extents(id).map_err(Self::error)?;
            // The extent the slot names first, so that of two holding one
            // dictionary, the other is released.
            extents.sort_by_key(|(extent, _)| current.is_none_or(|(at, _, _)| at != *extent));
            let mut kept = BTreeSet::new();
            let mut released = Vec::new();
            for (extent, dictionary) in extents {
                let wanted = current.is_some_and(|(_, held, _)| held == dictionary)
                    || named.contains(&dictionary);
                if wanted && kept.insert(dictionary) {
                    continue;
                }
                file.release_dictionary(id, extent).map_err(Self::error)?;
                if !wanted {
                    released.push(dictionary);
                }
            }
            Ok(released)
        })?;
        for dictionary in released {
            self.shared.frames.forget(dictionary);
        }
        Ok(())
    }

    /// The dictionaries the collection's records name, read from each
    /// frame's header without decompressing it, or `None` when some
    /// segment does not read to its end.
    fn named_by_records(&self) -> Result<Option<BTreeSet<u32>>, NativeStorageError> {
        let frames = self.frame_bodies();
        let mut named = BTreeSet::new();
        for number in self.segment_numbers()? {
            let Some(bytes) = self.read_segment(number)? else {
                continue;
            };
            if bytes.len() < LogSegmentHeader::ENCODED_BYTES {
                return Ok(None);
            }
            let Ok((bodies, end)) = LogScan::stored_bodies(&bytes, &*frames) else {
                return Ok(None);
            };
            if end != bytes.len() as u64 {
                return Ok(None);
            }
            for (_, stored) in bodies {
                match ContainerStoredBody::parse(stored) {
                    Ok(parsed) if parsed.dictionary_id != 0 => {
                        named.insert(parsed.dictionary_id);
                    }
                    Ok(_) => {}
                    Err(_) => return Ok(None),
                }
            }
        }
        Ok(Some(named))
    }
}

/// The bytes `samples` take stored with `dictionary`, or with none; `None`
/// when the dictionary does not load.
/// The body of `record` as a dictionary trains on it, or `None` for a
/// record that holds no document. A deletion's record carries only its
/// manifest, and would teach a dictionary nothing of the documents.
fn sample(record: &LogRecord) -> Option<Vec<u8>> {
    if record.kind != LogRecordKind::Commit || record.blobs.is_empty() {
        return None;
    }
    let encoded = record.encode().ok()?;
    Some(encoded[LogRecord::FRAME_BYTES..].to_vec())
}

fn stored_bytes(dictionary: Option<&ContainerDictionary>, samples: &[Vec<u8>]) -> Option<usize> {
    let mut codec = ContainerFrameCodec::new(dictionary.cloned()).ok()?;
    let mut total = 0;
    for sample in samples {
        total += codec.encode(sample).ok()?.bytes.len();
    }
    Some(total)
}
