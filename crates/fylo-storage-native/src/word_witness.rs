use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

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

type WitnessKey = (PathBuf, String);

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
        let key = (collection.path().to_path_buf(), field.to_owned());
        if let Some(complete) = Self::cached(&key, generation) {
            return Ok(complete);
        }
        let complete = Self::compare(collection, field)?;
        Self::remember(key, generation, complete);
        Ok(complete)
    }

    fn compare(collection: &NativeCollection, field: &str) -> Result<bool, NativeStorageError> {
        let (equality, witness) = word_witness_prefixes(field);
        let snapshot = collection.index_snapshot_in_ranges(&[
            IndexKeyRange::prefix(&equality),
            IndexKeyRange::prefix(&witness),
        ])?;
        let values = Self::suffixes(&snapshot, &equality);
        let witnesses = Self::suffixes(&snapshot, &witness);
        Ok(values == witnesses)
    }

    fn suffixes<'a>(snapshot: &'a IndexSnapshot, prefix: &str) -> BTreeSet<&'a [u8]> {
        snapshot
            .as_bytes()
            .split(|byte| *byte == b'\n')
            .filter_map(|key| key.strip_prefix(prefix.as_bytes()))
            .collect()
    }

    fn cached(key: &WitnessKey, generation: u64) -> Option<bool> {
        let cache = Self::cache().lock().ok()?;
        let (cached_generation, complete) = cache.get(key)?;
        (*cached_generation == generation).then_some(*complete)
    }

    fn remember(key: WitnessKey, generation: u64, complete: bool) {
        if let Ok(mut cache) = Self::cache().lock() {
            cache.insert(key, (generation, complete));
        }
    }

    fn cache() -> &'static Mutex<HashMap<WitnessKey, (u64, bool)>> {
        static CACHE: OnceLock<Mutex<HashMap<WitnessKey, (u64, bool)>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }
}
