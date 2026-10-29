//! Lazy migration on read and convergence (ADR 0017 sections 5 and 6).
//!
//! Every document a read engine returns passes through
//! [`ReadOnlyEngine::decode_document`]. A document stored below its
//! collection's head version is decrypted at the version it was written at
//! and migrated in memory by the JSON steps of every later version; the
//! stored bytes do not change. A document at the head costs one comparison
//! of its `_v`.
//!
//! The prefix index is built from stored documents, so while a collection
//! has documents below the head it can be wrong for the fields their
//! migrations write. A query whose index plan touches such a field reads
//! every document instead; one that touches none, or any query over a
//! converged collection, keeps the index.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use fylo_storage_native::{GenerationStatus, NativeCollection, SchemaVersionCounts};
use serde_json::{Map, Value};

use crate::encryption::reject_undeclared_ciphertext;
use crate::equality_plan::EqualityPlan;
use crate::schema::VersionChain;
use crate::{EngineError, ReadOnlyEngine};

/// Version chains a read engine has loaded, by collection. Like the heads its
/// registry remembers, one chain answers every read of one engine.
pub(crate) type ChainCache = Arc<Mutex<HashMap<String, Option<Arc<VersionChain>>>>>;
/// Counts a read engine took by scan, by collection and the stable
/// generation they describe.
pub(crate) type CountCache = Arc<Mutex<HashMap<(String, u64), SchemaVersionCounts>>>;

/// The top-level fields an index plan must not use while a collection has
/// documents below its head.
#[derive(Clone, Debug)]
pub(crate) struct MigrationGuard {
    fields: BTreeSet<String>,
}

impl MigrationGuard {
    /// Whether every field `plan` reads from the index means the same in
    /// stored and migrated documents.
    pub(crate) fn allows(&self, plan: &EqualityPlan) -> bool {
        plan.fields().all(|field| !self.fields.contains(field))
    }
}

/// Where a collection stands against its head (ADR 0017 section 6).
pub(crate) struct Convergence;

impl Convergence {
    /// The oldest version that live documents are stored at below the head,
    /// or `None` when every document is at the head. A document without
    /// `_v` is at the oldest version; one at a label the chain does not know
    /// is treated as the oldest too.
    pub(crate) fn oldest_behind<'a>(
        chain: &'a VersionChain,
        counts: &'a SchemaVersionCounts,
    ) -> Option<&'a str> {
        if counts.unversioned > 0 {
            return Some(chain.oldest());
        }
        let labels = chain.labels();
        if let Some(unknown) = counts
            .versions
            .iter()
            .find(|(label, count)| **count > 0 && !labels.contains(label))
        {
            return Some(unknown.0);
        }
        labels
            .iter()
            .find(|label| label.as_str() != chain.head() && counts.at(label) > 0)
            .map(String::as_str)
    }

    /// Documents at each of `labels`, in order, the unversioned ones counted
    /// at the first; documents at labels outside `labels`; and whether every
    /// document is at `head`.
    pub(crate) fn report(
        labels: &[String],
        head: Option<&str>,
        counts: &SchemaVersionCounts,
    ) -> (Vec<u64>, BTreeMap<String, u64>, bool) {
        let per_version: Vec<u64> = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                counts.at(label) + if index == 0 { counts.unversioned } else { 0 }
            })
            .collect();
        let unknown = counts
            .versions
            .iter()
            .filter(|(label, _)| !labels.contains(label))
            .map(|(label, count)| (label.clone(), *count))
            .collect();
        let at_head = match head {
            Some(head) => labels
                .iter()
                .position(|label| label == head)
                .map_or(0, |index| per_version[index]),
            None => counts.total(),
        };
        (per_version, unknown, at_head == counts.total())
    }
}

impl ReadOnlyEngine {
    /// `collection`'s version chain, loaded once per engine.
    pub(crate) fn version_chain(
        &self,
        collection: &str,
    ) -> Result<Option<Arc<VersionChain>>, EngineError> {
        if let Some(chain) = lock(&self.chains).get(collection) {
            return Ok(chain.clone());
        }
        let chain = VersionChain::load(&self.registry, self.schema_dir.as_deref(), collection)
            .map_err(EngineError::schema)?
            .map(Arc::new);
        lock(&self.chains).insert(collection.to_owned(), chain.clone());
        Ok(chain)
    }

    /// The fields a read returns for stored `fields`: decrypted, and in the
    /// head shape unless this engine reads documents as stored.
    pub(crate) fn present_fields(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, EngineError> {
        if !self.stored_shape
            && let Some(chain) = self.version_chain(collection)?
            && !chain.is_at_head(&fields)
        {
            let version = chain.written_at(&fields).to_owned();
            let decrypted = self.decrypt_fields(collection, fields, Some(&version))?;
            return chain
                .migrate(collection, &decrypted)
                .map_err(EngineError::schema);
        }
        self.decrypt_fields(collection, fields, None)
    }

    /// Decrypt with the `$encrypted` list of `version`, else of the version
    /// the document's `_v` names.
    pub(crate) fn decrypt_fields(
        &self,
        collection: &str,
        fields: Map<String, Value>,
        version: Option<&str>,
    ) -> Result<Map<String, Value>, EngineError> {
        match (&self.encryption, version) {
            (Some(encryption), Some(version)) => encryption
                .decode_document_at(collection, fields, Some(version))
                .map_err(EngineError::encryption),
            (Some(encryption), None) => encryption
                .decode_document(collection, fields)
                .map_err(EngineError::encryption),
            (None, _) => {
                reject_undeclared_ciphertext(collection, &fields)
                    .map_err(EngineError::encryption)?;
                Ok(fields)
            }
        }
    }

    /// Live documents of `collection` by stored version: the count the
    /// writer recorded at the current generation, else a scan, kept for
    /// this engine's lifetime at the generation it describes.
    pub(crate) fn schema_version_counts(
        &self,
        collection: &NativeCollection,
    ) -> Result<SchemaVersionCounts, EngineError> {
        if let Some(counts) = collection
            .recorded_schema_versions()
            .map_err(EngineError::storage)?
        {
            return Ok(counts);
        }
        let before = Self::stable_generation(collection)?;
        let key = (collection.name().to_owned(), before.generation);
        if let Some(counts) = lock(&self.counts).get(&key) {
            return Ok(counts.clone());
        }
        let counts = collection
            .scan_schema_versions()
            .map_err(EngineError::storage)?;
        let after = Self::read_attempt_generation(collection)?;
        if after.is_some_and(|after| {
            after.state == GenerationStatus::Stable && after.generation == before.generation
        }) {
            lock(&self.counts).insert(key, counts.clone());
        }
        Ok(counts)
    }

    /// What an index plan over `collection` must avoid, or `None` when the
    /// index is right for every field: no migrations, a converged
    /// collection, or an engine that reads documents as stored.
    pub(crate) fn migration_guard(
        &self,
        collection: &NativeCollection,
    ) -> Result<Option<MigrationGuard>, EngineError> {
        if self.stored_shape {
            return Ok(None);
        }
        let Some(chain) = self.version_chain(collection.name())? else {
            return Ok(None);
        };
        let counts = self.schema_version_counts(collection)?;
        let Some(oldest) = Convergence::oldest_behind(&chain, &counts) else {
            return Ok(None);
        };
        let fields = chain
            .fields_migrated_from(collection.name(), oldest)
            .map_err(EngineError::schema)?;
        Ok(Some(MigrationGuard { fields }))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
