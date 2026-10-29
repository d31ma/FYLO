use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde_json::{Map, Value};

use crate::{NativeStorageError, NativeStorageErrorCode, ValidationMode};

/// What happens to a document body between storage and the mutation that
/// rewrites it.
///
/// Every mutation that stores a body builds it through a [`DocumentPolicy`],
/// which runs these steps in the order ADR 0017 section 1 fixes. Storage
/// computes some post-images itself (a shallow patch merges into the stored
/// body, and SQL `INSERT` and `UPDATE` build bodies from a statement), and the
/// engine owns the schema and field encryption, so the engine hands storage a
/// codec. `decode` turns a stored body into the fields the mutation works on
/// (decrypting declared fields), `validate` checks a post-image against the
/// head schema, and `encode` turns the post-image into the bytes to store
/// (stamping `_v`, encrypting and encoding). Whether `validate` runs, and what
/// a failure does, is the collection's validation mode.
pub trait DocumentCodec {
    /// Fields a mutation should see for a stored body of `collection`.
    ///
    /// # Errors
    ///
    /// Returns an error when the body cannot be decoded, for example a
    /// declared field that does not decrypt.
    fn decode(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, NativeStorageError>;

    /// Why the post-image `fields` of `collection` does not match its head
    /// schema, or `None` when it does or the collection has no schema.
    ///
    /// # Errors
    ///
    /// Returns an error when the check itself cannot run.
    fn validate(
        &self,
        collection: &str,
        fields: &Map<String, Value>,
    ) -> Result<Option<String>, NativeStorageError>;

    /// Canonical bytes to store for the post-image `fields` of `collection`.
    ///
    /// # Errors
    ///
    /// Returns an error when the post-image cannot be encoded.
    fn encode(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Vec<u8>, NativeStorageError>;

    /// Whether `decode` can change what a query over `collection` sees in a
    /// stored body, so that a SQL `UPDATE` or `DELETE` must match its `WHERE`
    /// against decoded bodies, as a read would (ADR 0017 section 5): a
    /// declared encrypted field, or documents below the head schema version.
    ///
    /// # Errors
    ///
    /// Returns an error when the collection's schema cannot be read.
    fn decodes_for_query(&self, _collection: &str) -> Result<bool, NativeStorageError> {
        Ok(false)
    }
}

/// The codec for a writer with no schema or encryption: bodies pass through
/// and are only checked against the document format.
pub struct PlainDocumentCodec;

impl DocumentCodec for PlainDocumentCodec {
    fn decode(
        &self,
        _collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, NativeStorageError> {
        Ok(fields)
    }

    fn validate(
        &self,
        _collection: &str,
        _fields: &Map<String, Value>,
    ) -> Result<Option<String>, NativeStorageError> {
        Ok(None)
    }

    fn encode(
        &self,
        _collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Vec<u8>, NativeStorageError> {
        super::encode_document_fields(fields)
    }
}

/// Documents each collection accepted in `warn` mode although its schema
/// rejected them, since this process started, keyed by root and collection.
static SCHEMA_VIOLATIONS: Mutex<BTreeMap<(PathBuf, String), u64>> = Mutex::new(BTreeMap::new());

/// Writes to `collection` under `root` that this process committed in `warn`
/// mode although the schema rejected them.
///
/// The count lives in memory: it starts at zero in every process, and a
/// one-shot CLI invocation always reports zero. `fylo schema check` finds the
/// documents themselves.
#[must_use]
pub fn schema_violations(root: &Path, collection: &str) -> u64 {
    SCHEMA_VIOLATIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&(root.to_path_buf(), collection.to_owned()))
        .copied()
        .unwrap_or(0)
}

/// The record builder's schema policy for one mutation of one collection
/// (ADR 0017 sections 1 and 2).
///
/// It wraps the caller's codec and is the only way a mutation encodes a body:
/// in `strict` mode a post-image the schema rejects fails the mutation with
/// `ESCHEMA` before anything is written; in `warn` mode it is encoded anyway
/// and the violation is held until the mutation commits, then counted and
/// logged; in `off` mode nothing is checked. `_v` stamping and encryption are
/// the codec's `encode` and run in every mode.
pub(crate) struct DocumentPolicy<'a> {
    codec: &'a dyn DocumentCodec,
    mode: ValidationMode,
    root: PathBuf,
    collection: String,
    violations: RefCell<Vec<String>>,
}

impl<'a> DocumentPolicy<'a> {
    pub(crate) fn new(
        codec: &'a dyn DocumentCodec,
        mode: ValidationMode,
        root: &Path,
        collection: &str,
    ) -> Self {
        Self {
            codec,
            mode,
            root: root.to_path_buf(),
            collection: collection.to_owned(),
            violations: RefCell::new(Vec::new()),
        }
    }

    /// Forget the violations of an attempt that is about to be planned again.
    ///
    /// A logged commit may plan its mutation more than once, and a SQL
    /// `UPDATE` too large for the log is planned again for the journal; only
    /// the attempt that commits may count.
    pub(crate) fn restart(&self) {
        self.violations.borrow_mut().clear();
    }

    /// Count and log the violations `warn` mode let through, once the
    /// mutation has committed.
    pub(crate) fn finish<T>(
        self,
        outcome: Result<T, NativeStorageError>,
    ) -> Result<T, NativeStorageError> {
        let value = outcome?;
        let violations = self.violations.into_inner();
        if !violations.is_empty() {
            *SCHEMA_VIOLATIONS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry((self.root, self.collection.clone()))
                .or_default() += violations.len() as u64;
            for reason in violations {
                eprintln!(
                    "fylo: warning: collection \"{}\" stored a document its schema rejects \
                     (validation: warn): {reason}",
                    self.collection
                );
            }
        }
        Ok(value)
    }
}

impl DocumentCodec for DocumentPolicy<'_> {
    fn decode(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, NativeStorageError> {
        self.codec.decode(collection, fields)
    }

    fn validate(
        &self,
        collection: &str,
        fields: &Map<String, Value>,
    ) -> Result<Option<String>, NativeStorageError> {
        self.codec.validate(collection, fields)
    }

    fn decodes_for_query(&self, collection: &str) -> Result<bool, NativeStorageError> {
        self.codec.decodes_for_query(collection)
    }

    fn encode(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Vec<u8>, NativeStorageError> {
        if self.mode != ValidationMode::Off
            && let Some(reason) = self.codec.validate(collection, &fields)?
        {
            if self.mode == ValidationMode::Strict {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::SchemaViolation,
                    reason,
                ));
            }
            self.violations.borrow_mut().push(reason);
        }
        self.codec.encode(collection, fields)
    }
}
