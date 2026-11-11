use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fylo_query::{IndexKeyRange, IndexSnapshot, word_witness_prefixes};

use crate::{NativeCollection, NativeStorageError};

/// Whether a word field's word keys are complete (ADR 0023 section 8).
///
/// Every release keeps a field's `eq` keys up to date; only a release that
/// knows the field is a word field also writes its `wv` witnesses, in the
/// same delta as its `w` keys. The field's word keys are complete exactly
/// when both runs hold the same tokens and identifiers. The answer is cached
/// per collection, field and generation, since every writer moves the
/// generation.
pub(crate) struct WordWitness;

/// One cached answer: collection path, field, generation, complete.
type WitnessEntry = (PathBuf, String, u64, bool);

impl WordWitness {
    /// The word fields of `collection` whose word keys are complete.
    pub(crate) fn complete_fields(
        collection: &NativeCollection,
    ) -> Result<Vec<String>, NativeStorageError> {
        let mut complete = Vec::new();
        if collection.word_fields().is_empty() {
            return Ok(complete);
        }
        let generation = collection.generation()?.generation;
        for field in collection.word_fields() {
            if Self::is_complete(collection, field, generation)? {
                complete.push(field.clone());
            }
        }
        Ok(complete)
    }

    fn is_complete(
        collection: &NativeCollection,
        field: &str,
        generation: u64,
    ) -> Result<bool, NativeStorageError> {
        if let Some(complete) = Self::cached(collection.path(), field, generation) {
            return Ok(complete);
        }
        let complete = Self::compare(collection, field)?;
        Self::remember((
            collection.path().to_path_buf(),
            field.to_owned(),
            generation,
            complete,
        ));
        Ok(complete)
    }

    fn compare(collection: &NativeCollection, field: &str) -> Result<bool, NativeStorageError> {
        let (equality, witness) = word_witness_prefixes(field);
        let snapshot = collection.index_snapshot_in_ranges(&[
            IndexKeyRange::prefix(&equality),
            IndexKeyRange::prefix(&witness),
        ])?;
        // The snapshot is sorted, so each run is too, and equal runs mean
        // equal sets.
        let values = Self::suffixes(&snapshot, &equality);
        let witnesses = Self::suffixes(&snapshot, &witness);
        Ok(values.eq(witnesses))
    }

    fn suffixes<'a>(
        snapshot: &'a IndexSnapshot,
        prefix: &'a str,
    ) -> impl Iterator<Item = &'a [u8]> + 'a {
        snapshot
            .as_bytes()
            .split(|byte| *byte == b'\n')
            .filter_map(move |key| key.strip_prefix(prefix.as_bytes()))
    }

    fn cached(path: &Path, field: &str, generation: u64) -> Option<bool> {
        let cache = Self::cache().lock().ok()?;
        cache
            .iter()
            .find(|entry| entry.0 == path && entry.1 == field && entry.2 == generation)
            .map(|entry| entry.3)
    }

    fn remember(entry: WitnessEntry) {
        if let Ok(mut cache) = Self::cache().lock() {
            cache.retain(|kept| kept.0 != entry.0 || kept.1 != entry.1);
            cache.push(entry);
        }
    }

    fn cache() -> &'static Mutex<Vec<WitnessEntry>> {
        static CACHE: Mutex<Vec<WitnessEntry>> = Mutex::new(Vec::new());
        &CACHE
    }
}
