//! FYLO engine orchestration.
//!
//! The current vertical slice is deliberately read-only. It combines native
//! storage discovery with the portable format and query kernels and verifies a
//! stable collection generation around every logical read.

mod encryption;
mod equality_operation;
mod equality_plan;
mod hot_set;
mod lazy_migration;
mod schema;
mod schema_admin;
#[cfg(any(test, target_arch = "wasm32"))]
mod stored_validation;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use encryption::{EncryptionReader, is_encrypted_field, reject_undeclared_ciphertext};
use fylo_format::{
    CanonicalMetadata, Document, DocumentLimits, FormatError, FormatErrorCode, TtidTimestamps,
};
use fylo_query::{
    IndexLookupValue, JoinSpec, QueryError, QueryErrorCode, QueryLimits, ScanQuery, SqlOperation,
    SqlPlan, StructuredQuery, index_entries_for_document,
};
use fylo_storage_native::{
    AccessDescriptor, CollectionKind, CollectionLayout, DocumentCodec, GenerationState,
    GenerationStatus, IndexVerification, NativeAccess, NativeCollection, NativeRoot,
    NativeStorageError, NativeStorageErrorCode, NativeWriteRoot, PutDocumentOptions,
    RepositoryHistory, RootConfig, RootOpenOptions, SqlMutationResult, StoredBytes, StoredRawFile,
    ValidationMode, VersionVerification, WriteAccess, WriteActor,
};
use hot_set::HotReader;
pub use hot_set::{HotSet, HotSetStatistics};
use lazy_migration::{ChainCache, CountCache};
use schema::{SchemaRegistry, SchemaTools, VersionChain};
use serde::Serialize;
use serde_json::{Map, Value, json};

const MAX_STABLE_READ_ATTEMPTS: usize = 3;
/// Drifted index keys reported by one verification.
const MAX_DRIFT_SAMPLE: usize = 12;

/// Read-only native FYLO engine.
#[derive(Clone)]
pub struct ReadOnlyEngine {
    root: NativeRoot,
    encryption: Option<EncryptionReader>,
    hot_set: Option<Arc<HotSet>>,
    registry: SchemaRegistry,
    /// The `fylo.schema` folder, for collections without a registry.
    schema_dir: Option<PathBuf>,
    /// Return documents as stored rather than migrated to the head.
    stored_shape: bool,
    chains: ChainCache,
    counts: CountCache,
}

/// Native FYLO write engine.
///
/// The engine owns schema-declared field encryption and canonical document
/// encoding; `fylo-storage-native` owns durability. Encryption is applied
/// before any byte reaches the transaction journal so a crash can never leave
/// plaintext behind for a declared field.
#[derive(Clone)]
pub struct WriteEngine {
    writer: NativeWriteRoot,
    encryption: Option<EncryptionReader>,
    schema: Option<std::rc::Rc<SchemaTools>>,
    registry: SchemaRegistry,
}

pub use fylo_storage_native::RootConfig as EngineConfig;

impl WriteEngine {
    /// Attach schema tooling and field encryption to the already opened root.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid schema metadata or encryption credentials.
    pub fn with_schema_configuration(
        mut self,
        schema_root: impl AsRef<Path>,
        credentials: Option<(&str, &str)>,
    ) -> Result<Self, EngineError> {
        let schema_root = schema_root.as_ref().to_path_buf();
        self.encryption = Some(
            EncryptionReader::open(&schema_root, credentials, self.registry.clone())
                .map_err(EngineError::encryption)?,
        );
        self.schema = Some(std::rc::Rc::new(SchemaTools::new(schema_root)));
        Ok(self)
    }

    /// Attach field encryption keys without a `fylo.schema` folder, for
    /// collections whose schemas live in the root's registry.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid encryption credentials.
    pub fn with_encryption_credentials(
        mut self,
        secret: &str,
        salt: &str,
    ) -> Result<Self, EngineError> {
        self.encryption = Some(Self::rekeyed(
            self.encryption.take(),
            &self.registry,
            (secret, salt),
        )?);
        Ok(self)
    }

    fn rekeyed(
        reader: Option<EncryptionReader>,
        registry: &SchemaRegistry,
        credentials: (&str, &str),
    ) -> Result<EncryptionReader, EngineError> {
        reader
            .unwrap_or_else(|| EncryptionReader::unconfigured(registry.clone()))
            .with_credentials(Some(credentials), None)
            .map_err(EngineError::encryption)
    }

    /// Apply host-supplied runtime knobs to this engine and its writer.
    ///
    /// Every `open_*` constructor starts from the default, so a host that
    /// wants non-default behavior chains this rather than choosing between a
    /// combinatorial set of constructors.
    #[must_use]
    pub fn with_config(mut self, config: RootConfig) -> Self {
        self.writer = self.writer.with_config(config);
        self
    }

    /// Runtime knobs this engine was opened with.
    #[must_use]
    pub fn config(&self) -> RootConfig {
        self.writer.config()
    }
}

impl WriteEngine {
    /// Open a write engine without field encryption.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be opened safely.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EngineError> {
        Self::open_profiled(RootOpenOptions::new(path))
    }

    /// Open a root using its explicit metadata profile and owning lease.
    ///
    /// # Errors
    ///
    /// Returns an error before recovery or writes for a mismatched profile,
    /// invalid lease, or unsafe root.
    pub fn open_profiled(options: RootOpenOptions) -> Result<Self, EngineError> {
        NativeWriteRoot::open_profiled(options)
            .map(Self::from_writer)
            .map_err(EngineError::storage)
    }

    /// A write engine over a root the caller already opened.
    #[must_use]
    pub fn from_writer(writer: NativeWriteRoot) -> Self {
        let registry =
            SchemaRegistry::directory(writer.native_root().clone(), Some(writer.clone()));
        Self {
            writer,
            encryption: Some(EncryptionReader::unconfigured(registry.clone())),
            schema: None,
            registry,
        }
    }

    /// Open a materialized branch worktree with catalog and repository
    /// metadata owned by the canonical root.
    ///
    /// # Errors
    ///
    /// Returns an error when either root cannot be opened safely.
    pub fn open_with_repository(
        path: impl AsRef<Path>,
        repository_root: impl AsRef<Path>,
    ) -> Result<Self, EngineError> {
        Self::open_profiled(RootOpenOptions::new(path).with_repository(repository_root))
    }

    /// Open a write engine with schema tooling but no decryption key.
    ///
    /// Schema inspection and validation work; a collection that declares
    /// `$encrypted` fields fails closed until credentials are supplied.
    ///
    /// # Errors
    ///
    /// Returns an error when either root cannot be opened safely.
    pub fn open_with_schema(
        path: impl AsRef<Path>,
        schema_root: impl AsRef<Path>,
    ) -> Result<Self, EngineError> {
        Self::open(path)?.with_schema_configuration(schema_root, None)
    }

    /// Open a materialized branch worktree with schema tooling.
    ///
    /// # Errors
    ///
    /// Returns an error when a root or schema boundary cannot be opened safely.
    pub fn open_with_repository_and_schema(
        path: impl AsRef<Path>,
        repository_root: impl AsRef<Path>,
        schema_root: impl AsRef<Path>,
    ) -> Result<Self, EngineError> {
        Self::open_with_repository(path, repository_root)?
            .with_schema_configuration(schema_root, None)
    }

    /// Open a write engine with JavaScript-compatible field encryption.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe roots, short/invalid credentials, corrupt
    /// schema metadata, or key derivation failures.
    pub fn open_with_encryption(
        path: impl AsRef<Path>,
        schema_root: impl AsRef<Path>,
        secret: &str,
        salt: &str,
    ) -> Result<Self, EngineError> {
        Self::open(path)?.with_schema_configuration(schema_root, Some((secret, salt)))
    }

    /// Open a materialized branch worktree with schema-driven encryption.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe roots, schema failures, or invalid
    /// encryption credentials.
    pub fn open_with_repository_and_encryption(
        path: impl AsRef<Path>,
        repository_root: impl AsRef<Path>,
        schema_root: impl AsRef<Path>,
        secret: &str,
        salt: &str,
    ) -> Result<Self, EngineError> {
        Self::open_with_repository(path, repository_root)?
            .with_schema_configuration(schema_root, Some((secret, salt)))
    }

    /// Canonical root identity.
    #[must_use]
    pub fn root_path(&self) -> &Path {
        self.writer.path()
    }

    /// Create one document, encrypting every schema-declared field first.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid document, missing encryption
    /// credentials, or any durable write failure.
    pub fn put_document(
        &self,
        collection: &str,
        identifier: &str,
        fields: Map<String, Value>,
        access: WriteAccess,
    ) -> Result<(), EngineError> {
        self.put_document_with_metadata(collection, identifier, fields, BTreeMap::new(), access)
    }

    /// Create one document with developer metadata in the same transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid document or metadata values, missing
    /// encryption credentials, or any durable write failure.
    pub fn put_document_with_metadata(
        &self,
        collection: &str,
        identifier: &str,
        fields: Map<String, Value>,
        metadata: BTreeMap<String, Value>,
        access: WriteAccess,
    ) -> Result<(), EngineError> {
        self.record_encrypted_fields(collection)?;
        let codec = EngineDocumentCodec::new(self);
        let outcome = self.writer.put_document_fields_with(
            collection,
            identifier,
            fields,
            PutDocumentOptions { metadata, access },
            &codec,
        );
        codec.finish(outcome)
    }

    /// Replace one document body, encrypting every schema-declared field first.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid document, missing encryption
    /// credentials, a denied write, or any durable write failure.
    pub fn patch_document(
        &self,
        collection: &str,
        identifier: &str,
        fields: Map<String, Value>,
        actor: Option<&AccessContext>,
    ) -> Result<(), EngineError> {
        self.record_encrypted_fields(collection)?;
        let codec = EngineDocumentCodec::new(self);
        let actor = actor.map(write_actor);
        let outcome =
            self.writer
                .patch_document_with(collection, identifier, fields, actor.as_ref(), &codec);
        codec.finish(outcome)
    }

    /// Shallow-merge `changes` into one stored document, then validate and
    /// encrypt the merged document exactly as a put would.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing document, a denied write, a stored
    /// field that does not decrypt, a merged document the schema rejects, or
    /// any durable write failure.
    pub fn patch_document_fields(
        &self,
        collection: &str,
        identifier: &str,
        changes: &Map<String, Value>,
        actor: Option<&AccessContext>,
    ) -> Result<(), EngineError> {
        self.record_encrypted_fields(collection)?;
        let codec = EngineDocumentCodec::new(self);
        let actor = actor.map(write_actor);
        let outcome = self.writer.patch_document_fields_with(
            collection,
            identifier,
            changes,
            actor.as_ref(),
            &codec,
        );
        codec.finish(outcome)
    }

    /// Execute a SQL `INSERT`, `UPDATE` or `DELETE`, validating and encrypting
    /// every row it writes exactly as a put would.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed SQL, a denied record, a row the schema
    /// rejects, a stored field that does not decrypt, or any durable write
    /// failure.
    pub fn execute_sql_mutation(
        &self,
        sql: &str,
        actor: Option<&AccessContext>,
        insert_access: WriteAccess,
    ) -> Result<SqlMutationResult, EngineError> {
        let codec = EngineDocumentCodec::new(self);
        let actor = actor.map(write_actor);
        let outcome =
            self.writer
                .execute_sql_mutation_with(sql, actor.as_ref(), insert_access, &codec);
        // The statement names its collection, so the record follows the
        // rows it encoded rather than leading them.
        let encoded = codec.encoded.take();
        let result = codec.finish(outcome)?;
        for collection in encoded {
            self.record_encrypted_fields(&collection)?;
        }
        Ok(result)
    }

    /// Bring `collection`'s encrypted-fields record up to the `$encrypted`
    /// list this engine's `fylo.schema` folder declares, when the engine has
    /// the key and the collection no registry, so a writer without the
    /// folder encrypts those fields too, or is refused without the key. A
    /// collection that does not exist records nothing; the write that
    /// follows reports it.
    ///
    /// # Errors
    ///
    /// Returns an error for an unreadable schema folder, lock contention,
    /// or an interrupted durable write.
    pub fn record_encrypted_fields(&self, collection: &str) -> Result<(), EngineError> {
        let Some(encryption) = self.encryption.as_ref() else {
            return Ok(());
        };
        match encryption.record_folder_declaration(collection) {
            Err(error)
                if matches!(
                    error.storage_code(),
                    Some(
                        NativeStorageErrorCode::NotFound
                            | NativeStorageErrorCode::InvalidCollection
                    )
                ) =>
            {
                Ok(())
            }
            outcome => outcome,
        }
    }

    /// Rewrite one stored document at the head schema version: read it,
    /// apply the JSON migrations from its `_v` (ADR 0018), then validate and
    /// encrypt it as a put would, in one read-modify-write (ADR 0017
    /// section 5, eager migration).
    ///
    /// # Errors
    ///
    /// Returns an error when no schema root is configured, the document is
    /// missing or denied, a migration fails, or the head schema rejects the
    /// migrated document.
    pub fn migrate_document(
        &self,
        collection: &str,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<(), EngineError> {
        if self.registry_head(collection)?.is_none() {
            self.schema_tools()?;
        }
        // Every pre-image is migrated to the head as it is decoded, so an
        // empty patch is the migration.
        self.patch_document_fields(collection, identifier, &Map::new(), actor)
    }

    /// `collection`'s version chain as this engine sees it now: a write
    /// engine reads the head on every write.
    fn version_chain(&self, collection: &str) -> Result<Option<VersionChain>, EngineError> {
        VersionChain::load(&self.registry, self.schema_folder(), collection)
            .map_err(EngineError::schema)
    }

    /// A stored document as a mutation sees it: decrypted, and migrated to
    /// the head when it is below it (ADR 0017 section 1, step 1), exactly
    /// as a read returns it.
    fn decode_fields_at_head(
        &self,
        chain: Option<&VersionChain>,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, EngineError> {
        match chain {
            Some(chain) if !chain.is_at_head(&fields) => {
                let version = chain.written_at(&fields).to_owned();
                let decoded = self.decode_fields_at(collection, fields, &version)?;
                chain
                    .migrate(collection, &decoded)
                    .map_err(EngineError::schema)
            }
            _ => self.decode_fields(collection, fields),
        }
    }

    fn decode_fields_at(
        &self,
        collection: &str,
        fields: Map<String, Value>,
        version: &str,
    ) -> Result<Map<String, Value>, EngineError> {
        match self.encryption.as_ref() {
            Some(encryption) => encryption
                .decode_document_at(collection, fields, Some(version))
                .map_err(EngineError::encryption),
            None => self.decode_fields(collection, fields),
        }
    }

    /// [`Self::decode_fields`] for a document about to be migrated, which
    /// schema materialization takes to be at the oldest version when it has
    /// no `_v`; it is decrypted as that version, too.
    fn decode_fields_as_written(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, EngineError> {
        let Some(encryption) = self.encryption.as_ref() else {
            return self.decode_fields(collection, fields);
        };
        let version = match fields.get("_v").and_then(Value::as_str) {
            Some(version) => Some(version.to_owned()),
            None => encryption
                .oldest_version(collection)
                .map_err(EngineError::encryption)?,
        };
        encryption
            .decode_document_at(collection, fields, version.as_deref())
            .map_err(EngineError::encryption)
    }

    fn decode_fields(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, EngineError> {
        if let Some(encryption) = self.encryption.as_ref() {
            encryption
                .decode_document(collection, fields)
                .map_err(EngineError::encryption)
        } else {
            reject_undeclared_ciphertext(collection, &fields).map_err(EngineError::encryption)?;
            Ok(fields)
        }
    }

    /// The validation mode writes to `collection` run under (ADR 0017
    /// section 2).
    ///
    /// # Errors
    ///
    /// Returns an error for a missing collection or a corrupt descriptor.
    pub fn validation_mode(&self, collection: &str) -> Result<ValidationMode, EngineError> {
        self.writer
            .validation_mode(collection)
            .map_err(EngineError::storage)
    }

    /// Record `mode` in `collection`'s catalog descriptor. Returns the mode
    /// the descriptor recorded before, if any.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing collection or a failed durable write.
    pub fn set_validation_mode(
        &self,
        collection: &str,
        mode: ValidationMode,
    ) -> Result<Option<ValidationMode>, EngineError> {
        self.writer
            .set_validation_mode(collection, mode)
            .map_err(EngineError::storage)
    }

    /// `fylo schema check`: every live document of `collection` the head
    /// schema rejects, whatever the collection's validation mode, so a
    /// collection in `warn` mode can be brought into line before it becomes
    /// `strict`. At most `limit` failures are listed; `failed` counts all.
    ///
    /// # Errors
    ///
    /// Returns an error when no schema root is configured, the collection is
    /// missing or not a document collection, or a stored document cannot be
    /// read or decrypted.
    pub fn schema_check(&self, collection: &str, limit: usize) -> Result<Value, EngineError> {
        let current = self.schema_current(collection)?;
        #[cfg(target_arch = "wasm32")]
        self.require_stored_validator(collection)?;
        let handle = self
            .writer
            .collection(collection)
            .map_err(EngineError::storage)?;
        let identifiers = handle.document_ids().map_err(EngineError::storage)?;
        let chain = self.version_chain(collection)?;
        let mut failures = Vec::new();
        let mut failed = 0_usize;
        for identifier in &identifiers {
            let stored = match handle.read_document(identifier) {
                Ok(stored) => stored,
                // Deleted since it was listed.
                Err(error) if error.code() == NativeStorageErrorCode::NotFound => continue,
                Err(error) => return Err(EngineError::storage(error)),
            };
            let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(&stored.bytes) else {
                return Err(EngineError::new(
                    EngineErrorCode::CorruptData,
                    format!("stored document {identifier} is not a JSON object"),
                ));
            };
            // Checked as a read returns it: an old document that migrates to
            // a valid head document passes.
            let fields = self.decode_fields_at_head(chain.as_ref(), collection, fields)?;
            if let Some(reason) = self.violation(collection, &fields) {
                failed += 1;
                if failures.len() < limit {
                    failures.push(json!({ "id": identifier, "error": reason }));
                }
            }
        }
        Ok(json!({
            "collection": collection,
            "schemaDir": self.schema_folder(),
            "current": current,
            "validation": self.validation_mode(collection)?.as_str(),
            "checked": identifiers.len(),
            "failed": failed,
            "documents": failures,
            "truncated": failed > limit,
        }))
    }

    /// `schemaInspect` for one collection.
    ///
    /// # Errors
    ///
    /// Returns an error when no schema root is configured or the manifest is
    /// corrupt.
    pub fn schema_inspect(&self, collection: &str) -> Result<Value, EngineError> {
        let report = if self.registry_head(collection)?.is_some() {
            self.registry_inspection(collection)?
        } else {
            self.schema_tools()?
                .inspect(collection)
                .map_err(EngineError::schema)?
        };
        self.with_convergence(collection, report)
    }

    /// `schemaDoctor` for one collection.
    ///
    /// # Errors
    ///
    /// Returns an error when no schema root is configured.
    pub fn schema_doctor(&self, collection: &str) -> Result<Value, EngineError> {
        if self.registry_head(collection)?.is_some() {
            return self.registry_diagnosis(collection);
        }
        Ok(self.schema_tools()?.doctor(collection))
    }

    /// Head schema version label, or `None` for an unversioned collection.
    ///
    /// # Errors
    ///
    /// Returns an error when no schema root is configured or the manifest is
    /// corrupt.
    pub fn schema_current(&self, collection: &str) -> Result<Option<String>, EngineError> {
        if let Some(head) = self.registry_head(collection)? {
            return Ok(Some(head.label().to_owned()));
        }
        self.schema_tools()?
            .current_version(collection)
            .map_err(EngineError::schema)
    }

    /// Resolved schema root.
    ///
    /// # Errors
    ///
    /// Returns an error when no schema root is configured.
    pub fn schema_dir(&self) -> Result<&Path, EngineError> {
        Ok(self.schema_tools()?.schema_dir())
    }

    /// Validate one document against the head schema and stamp `_v`.
    ///
    /// # Errors
    ///
    /// Returns an error when no schema root is configured, the validator is
    /// unavailable, or the document does not match its schema.
    pub fn schema_validate(
        &self,
        collection: &str,
        document: &Map<String, Value>,
    ) -> Result<Map<String, Value>, EngineError> {
        #[cfg(target_arch = "wasm32")]
        self.require_stored_validator(collection)?;
        if let Some(validated) = self
            .registry
            .validate_against_head(collection, document)
            .map_err(EngineError::schema)?
        {
            return Ok(validated);
        }
        Ok(self
            .schema_tools()?
            .validate_against_head(collection, document)
            .map_err(EngineError::schema)?
            .unwrap_or_else(|| Document::copy_fields(document)))
    }

    /// Materialize one document at the head schema version by applying the
    /// collection's ordered chain of JSON migrations (ADR 0018).
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest is invalid, a migration is missing
    /// or malformed, or a step meets a field of the wrong type.
    pub fn schema_materialize(
        &self,
        collection: &str,
        document: &Map<String, Value>,
    ) -> Result<Map<String, Value>, EngineError> {
        if let Some(materialized) = self
            .registry
            .materialize(collection, document)
            .map_err(EngineError::schema)?
        {
            return Ok(materialized);
        }
        self.schema_tools()?
            .materialize(collection, document)
            .map_err(EngineError::schema)
    }

    fn schema_tools(&self) -> Result<&SchemaTools, EngineError> {
        self.schema.as_deref().ok_or_else(|| {
            EngineError::new(
                EngineErrorCode::Schema,
                "schema operations require fylo.schema configuration",
            )
        })
    }

    /// Why `fields` does not match `collection`'s head schema, if it does
    /// not. Storage decides from the collection's validation mode whether this
    /// runs and what a failure does.
    fn violation(&self, collection: &str, fields: &Map<String, Value>) -> Option<String> {
        match self.registry.validate_against_head(collection, fields) {
            Ok(Some(_)) => None,
            Err(reason) => Some(reason),
            Ok(None) => self
                .schema
                .as_ref()
                .and_then(|schema| schema.validate_against_head(collection, fields).err()),
        }
    }

    /// The bytes a write stores for the post-image `fields`: `_v` stamped,
    /// declared fields encrypted, canonically encoded. Validation is not part
    /// of it; storage runs [`Self::violation`] first, under the collection's
    /// validation mode, so the stamp and encryption apply in every mode.
    fn encode(&self, collection: &str, fields: Map<String, Value>) -> Result<Vec<u8>, EngineError> {
        let fields = if let Some(encryption) = self.encryption.as_ref() {
            // Stamp before encrypting: the stamp is plaintext metadata, and
            // encryption must see the post-image the schema describes. A
            // collection with a schema registry is stamped with its head
            // there, whether or not a `fylo.schema` folder is configured.
            let fields = match (self.registry_head(collection)?, self.schema.as_ref()) {
                (Some(head), _) => {
                    let mut fields = fields;
                    fields.insert(
                        SchemaRegistry::VERSION_FIELD.into(),
                        Value::String(head.label().to_owned()),
                    );
                    fields
                }
                (None, Some(schema)) => schema
                    .stamp_head(collection, fields)
                    .map_err(EngineError::schema)?,
                (None, None) => fields,
            };
            encryption
                .encode_document(collection, fields)
                .map_err(EngineError::encryption)?
        } else {
            reject_undeclared_ciphertext(collection, &fields).map_err(EngineError::encryption)?;
            fields
        };
        Document::try_from_value(Value::Object(fields), DocumentLimits::default())
            .and_then(|document| document.encode())
            .map_err(EngineError::format)
    }
}

/// The write engine's validation and encryption, handed to storage for the
/// mutations whose post-images storage computes itself (ADR 0017 section 1).
///
/// Storage reports codec failures as storage errors, so the engine error that
/// caused one is kept here and returned in its place, with its own code.
struct EngineDocumentCodec<'a> {
    engine: &'a WriteEngine,
    failure: std::cell::RefCell<Option<EngineError>>,
    /// Version chains read for this mutation, which holds its collection
    /// against other writers while it decodes.
    chains: std::cell::RefCell<BTreeMap<String, Option<Arc<VersionChain>>>>,
    /// Collections this mutation encoded a body for.
    encoded: std::cell::RefCell<std::collections::BTreeSet<String>>,
}

impl<'a> EngineDocumentCodec<'a> {
    fn new(engine: &'a WriteEngine) -> Self {
        Self {
            engine,
            failure: std::cell::RefCell::new(None),
            chains: std::cell::RefCell::new(BTreeMap::new()),
            encoded: std::cell::RefCell::default(),
        }
    }

    fn chain(&self, collection: &str) -> Result<Option<Arc<VersionChain>>, EngineError> {
        if let Some(chain) = self.chains.borrow().get(collection) {
            return Ok(chain.clone());
        }
        let chain = self.engine.version_chain(collection)?.map(Arc::new);
        self.chains
            .borrow_mut()
            .insert(collection.to_owned(), chain.clone());
        Ok(chain)
    }

    fn fail(&self, error: EngineError) -> NativeStorageError {
        let stand_in = NativeStorageError::codec_rejected(error.to_string());
        self.failure.replace(Some(error));
        stand_in
    }

    fn finish<T>(self, outcome: Result<T, NativeStorageError>) -> Result<T, EngineError> {
        outcome.map_err(|error| {
            self.failure
                .into_inner()
                .unwrap_or_else(|| EngineError::storage(error))
        })
    }
}

impl DocumentCodec for EngineDocumentCodec<'_> {
    fn decode(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Map<String, Value>, NativeStorageError> {
        self.chain(collection)
            .and_then(|chain| {
                self.engine
                    .decode_fields_at_head(chain.as_deref(), collection, fields)
            })
            .map_err(|error| self.fail(error))
    }

    fn decodes_for_query(&self, collection: &str) -> Result<bool, NativeStorageError> {
        let outcome = self.chain(collection).and_then(|chain| {
            if chain.is_some() {
                return Ok(true);
            }
            Ok(match self.engine.encryption.as_ref() {
                Some(encryption) => !encryption
                    .encrypted_fields(collection)
                    .map_err(EngineError::encryption)?
                    .is_empty(),
                None => false,
            })
        });
        outcome.map_err(|error| self.fail(error))
    }

    fn validate(
        &self,
        collection: &str,
        fields: &Map<String, Value>,
    ) -> Result<Option<String>, NativeStorageError> {
        // Storage asks only under `strict` and `warn`, and a build that
        // cannot validate refuses the write in both (ADR 0017 section 4).
        #[cfg(target_arch = "wasm32")]
        self.engine
            .require_stored_validator(collection)
            .map_err(|error| self.fail(error))?;
        Ok(self.engine.violation(collection, fields))
    }

    fn encode(
        &self,
        collection: &str,
        fields: Map<String, Value>,
    ) -> Result<Vec<u8>, NativeStorageError> {
        if !self.encoded.borrow().contains(collection) {
            self.encoded.borrow_mut().insert(collection.to_owned());
        }
        self.engine
            .encode(collection, fields)
            .map_err(|error| self.fail(error))
    }
}

fn write_actor(actor: &AccessContext) -> WriteActor {
    WriteActor::new(actor.uid, actor.groups.iter().copied())
}

/// Trusted actor identity supplied by a client or application boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessContext {
    uid: u32,
    groups: BTreeSet<u32>,
}

impl AccessContext {
    /// Construct an actor from a UID and trusted supplementary GIDs.
    #[must_use]
    pub fn new(uid: u32, groups: impl IntoIterator<Item = u32>) -> Self {
        Self {
            uid,
            groups: groups.into_iter().collect(),
        }
    }

    /// Trusted actor UID.
    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    /// Trusted supplementary group IDs.
    #[must_use]
    pub const fn groups(&self) -> &BTreeSet<u32> {
        &self.groups
    }
}

impl ReadOnlyEngine {
    const SEGMENT_READ_BATCH: usize = 256;

    /// Attach schema-driven decryption without changing the repository identity.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid schema metadata or encryption credentials.
    pub fn with_schema_configuration(
        mut self,
        schema_root: impl AsRef<Path>,
        credentials: Option<(&str, &str)>,
    ) -> Result<Self, EngineError> {
        self.encryption = Some(
            EncryptionReader::open(&schema_root, credentials, self.registry.clone())
                .map_err(EngineError::encryption)?,
        );
        self.schema_dir = Some(schema_root.as_ref().to_path_buf());
        self.chains = ChainCache::default();
        Ok(self)
    }

    /// Return documents exactly as they are stored, below the head version
    /// included, instead of migrated to the head (ADR 0017 section 5). Eager
    /// migration reads this way to find the documents it has to rewrite. The
    /// hot set holds migrated documents, so this engine does without it.
    #[must_use]
    pub fn as_stored(mut self) -> Self {
        self.stored_shape = true;
        self.hot_set = None;
        self
    }

    /// Attach decryption keys without a `fylo.schema` folder, for collections
    /// whose schemas live in the root's registry.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid encryption credentials.
    pub fn with_encryption_credentials(
        mut self,
        secret: &str,
        salt: &str,
    ) -> Result<Self, EngineError> {
        self.encryption = Some(WriteEngine::rekeyed(
            self.encryption.take(),
            &self.registry,
            (secret, salt),
        )?);
        Ok(self)
    }

    /// Answer document reads from `hot_set`, reading storage only for what
    /// is not resident there.
    ///
    /// See [`HotSet`] for the contract: the caller must be the root's only
    /// writer and clear the hot set after every change.
    #[must_use]
    pub fn with_hot_set(mut self, hot_set: Arc<HotSet>) -> Self {
        self.hot_set = Some(hot_set);
        self
    }

    /// Open an existing FYLO root without modifying it.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be opened safely.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EngineError> {
        Self::open_profiled(RootOpenOptions::new(path))
    }

    /// Open an existing root with an explicit profile and owning lease.
    ///
    /// This read-only constructor never initializes an EFS profile descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe roots, incompatible profiles, or invalid leases.
    pub fn open_profiled(options: RootOpenOptions) -> Result<Self, EngineError> {
        let root = NativeRoot::open_profiled(options).map_err(EngineError::storage)?;
        // One head per collection for this engine's lifetime: every read it
        // answers sees the same schema version.
        let registry = SchemaRegistry::directory(root.clone(), None).with_remembered_heads();
        Ok(Self {
            root,
            encryption: Some(EncryptionReader::unconfigured(registry.clone())),
            hot_set: None,
            registry,
            schema_dir: None,
            stored_shape: false,
            chains: ChainCache::default(),
            counts: CountCache::default(),
        })
    }

    /// Open a materialized branch worktree whose catalog and repository live
    /// at the canonical root.
    ///
    /// # Errors
    ///
    /// Returns an error when either root cannot be opened safely.
    pub fn open_with_repository(
        path: impl AsRef<Path>,
        repository_root: impl AsRef<Path>,
    ) -> Result<Self, EngineError> {
        Self::open_profiled(RootOpenOptions::new(path).with_repository(repository_root))
    }

    /// Open a read-only engine with a schema root but without a decryption key.
    ///
    /// This is useful for proving fail-closed behavior: collections declaring
    /// encrypted fields return `EENGINE_ENCRYPTION` until credentials are
    /// supplied.
    ///
    /// # Errors
    ///
    /// Returns an error when either root cannot be opened safely.
    pub fn open_with_schema(
        path: impl AsRef<Path>,
        schema_root: impl AsRef<Path>,
    ) -> Result<Self, EngineError> {
        Self::open(path)?.with_schema_configuration(schema_root, None)
    }

    /// Open a read-only engine with JavaScript-compatible field decryption.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe roots, short/invalid credentials, corrupt
    /// schema metadata, or key derivation failures.
    pub fn open_with_encryption(
        path: impl AsRef<Path>,
        schema_root: impl AsRef<Path>,
        secret: &str,
        salt: &str,
    ) -> Result<Self, EngineError> {
        Self::open(path)?.with_schema_configuration(schema_root, Some((secret, salt)))
    }

    /// Canonical root identity.
    #[must_use]
    pub fn root_path(&self) -> &Path {
        self.root.path()
    }

    /// Read the active repository's first-parent commit history.
    ///
    /// # Errors
    ///
    /// Returns a stable error for corrupt/unsafe repository metadata or an
    /// invalid limit.
    pub fn history(&self, limit: usize) -> Result<RepositoryHistory, EngineError> {
        self.root
            .version_history(limit)
            .map_err(EngineError::storage)
    }

    /// Verify the active head's reachable commit, tree, and blob integrity.
    ///
    /// # Errors
    ///
    /// Returns a stable error for corrupt/unsafe repository content or an
    /// exhausted verification bound.
    pub fn verify_history(&self, limit: usize) -> Result<VersionVerification, EngineError> {
        self.root
            .verify_version_history(limit)
            .map_err(EngineError::storage)
    }

    /// Read and validate one JSON document with canonical metadata.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe storage, corrupt documents, invalid
    /// identifiers, or concurrent write generations.
    pub fn get(&self, collection: &str, identifier: &str) -> Result<ReadDocument, EngineError> {
        self.get_with_access(collection, identifier, None)
    }

    /// Read one document as a trusted actor.
    ///
    /// # Errors
    ///
    /// Returns `EACCES` when the document's portable descriptor denies reads.
    pub fn get_as(
        &self,
        collection: &str,
        identifier: &str,
        actor: &AccessContext,
    ) -> Result<ReadDocument, EngineError> {
        self.get_with_access(collection, identifier, Some(actor))
    }

    fn get_with_access(
        &self,
        collection: &str,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<ReadDocument, EngineError> {
        match &self.hot_set {
            Some(hot_set) => HotReader::new(self, hot_set, collection).get(identifier, actor),
            None => self.get_from_storage(collection, identifier, actor),
        }
    }

    fn get_from_storage(
        &self,
        collection: &str,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<ReadDocument, EngineError> {
        let collection = self.open_collection(collection)?;
        Self::read_stable(&collection, || {
            self.read_document_with_access(&collection, identifier, actor)
        })
    }

    /// Whether a collection holds JSON documents or raw files.
    ///
    /// Unlike [`Self::inspect`], this lists nothing.
    ///
    /// # Errors
    ///
    /// Returns a stable error for a missing, unsafe or corrupt collection.
    pub fn collection_kind(&self, collection: &str) -> Result<CollectionKind, EngineError> {
        match &self.hot_set {
            Some(hot_set) => HotReader::new(self, hot_set, collection).kind(),
            None => Ok(self.open_collection(collection)?.kind()),
        }
    }

    fn open_collection(&self, collection: &str) -> Result<NativeCollection, EngineError> {
        self.root
            .collection(collection)
            .map_err(EngineError::storage)
    }

    /// Parse, decode and stamp one stored document.
    fn build_record(
        &self,
        collection: &str,
        identifier: &str,
        stored: &StoredBytes,
    ) -> Result<ReadDocument, EngineError> {
        let document = self.decode_document(
            collection,
            Document::parse(&stored.bytes, DocumentLimits::default())
                .map_err(EngineError::format)?,
        )?;
        let timestamps = TtidTimestamps::decode(identifier).map_err(EngineError::format)?;
        Ok(ReadDocument {
            metadata: CanonicalMetadata {
                id: identifier.to_owned(),
                created_at: timestamps.created_at,
                updated_at: stored.modified_millis,
                mtime: stored.modified_millis,
            },
            document,
        })
    }

    fn read_document_with_access(
        &self,
        collection: &NativeCollection,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<ReadDocument, EngineError> {
        let stored = collection
            .read_document(identifier)
            .map_err(EngineError::storage)?;
        require_read_access(stored.access, actor)?;
        self.build_record(collection.name(), identifier, &stored)
    }

    /// Canonical plus developer metadata for one live record.
    ///
    /// System fields win over colliding developer keys, so a caller always
    /// receives canonical identifiers and timestamps.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing record, corrupt metadata, an unsafe
    /// path, or denied access.
    pub fn metadata(&self, collection: &str, identifier: &str) -> Result<Value, EngineError> {
        self.metadata_with_access(collection, identifier, None)
    }

    /// Canonical plus developer metadata checked as one trusted actor.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::metadata`], including `EACCES`
    /// when the actor has no read bit through owner, group, or other.
    pub fn metadata_as(
        &self,
        collection: &str,
        identifier: &str,
        actor: &AccessContext,
    ) -> Result<Value, EngineError> {
        self.metadata_with_access(collection, identifier, Some(actor))
    }

    fn metadata_with_access(
        &self,
        collection: &str,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<Value, EngineError> {
        let handle = self
            .root
            .collection(collection)
            .map_err(EngineError::storage)?;
        Self::read_stable(&handle, || {
            if handle.kind() == CollectionKind::File {
                let stored = handle
                    .read_raw_file(identifier)
                    .map_err(EngineError::storage)?;
                require_read_access(stored.access_descriptor, actor)?;
                let file = build_read_file(identifier, stored)?;
                let mut merged = file.metadata.merge_with_custom(&file.custom_metadata);
                let descriptor = serde_json::to_value(&file.file).map_err(|error| {
                    EngineError::new(EngineErrorCode::CorruptData, error.to_string())
                })?;
                if let Some(fields) = descriptor.as_object() {
                    for (name, value) in fields {
                        merged.insert(name.clone(), value.clone());
                    }
                }
                return Ok(Value::Object(merged));
            }
            let record = self.read_document_with_access(&handle, identifier, actor)?;
            let custom: Map<String, Value> = handle
                .read_custom_metadata(identifier)
                .map_err(EngineError::storage)?
                .into_iter()
                .collect();
            Ok(Value::Object(record.metadata.merge_with_custom(&custom)))
        })
    }

    /// Read one unwrapped raw file with canonical, custom, and native metadata.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe storage, missing/corrupt xattrs,
    /// invalid identifiers, oversized files, or concurrent write generations.
    pub fn get_file(&self, collection: &str, identifier: &str) -> Result<ReadFile, EngineError> {
        self.get_file_with_access(collection, identifier, None)
    }

    /// Read one raw file as a trusted actor.
    ///
    /// # Errors
    ///
    /// Returns `EACCES` when the file's portable descriptor denies reads.
    pub fn get_file_as(
        &self,
        collection: &str,
        identifier: &str,
        actor: &AccessContext,
    ) -> Result<ReadFile, EngineError> {
        self.get_file_with_access(collection, identifier, Some(actor))
    }

    fn get_file_with_access(
        &self,
        collection: &str,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<ReadFile, EngineError> {
        let collection = self
            .root
            .collection(collection)
            .map_err(EngineError::storage)?;
        Self::read_stable(&collection, || {
            let stored = collection
                .read_raw_file(identifier)
                .map_err(EngineError::storage)?;
            require_read_access(stored.access_descriptor, actor)?;
            build_read_file(identifier, stored)
        })
    }

    /// Read one retained soft-deleted JSON document.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe/corrupt storage, invalid identifiers,
    /// or concurrent write generations.
    pub fn get_deleted(
        &self,
        collection: &str,
        identifier: &str,
    ) -> Result<ReadDeletedDocument, EngineError> {
        self.get_deleted_with_access(collection, identifier, None)
    }

    /// Read one retained deleted document as a trusted actor.
    ///
    /// # Errors
    ///
    /// Returns `EACCES` when the retained descriptor denies reads.
    pub fn get_deleted_as(
        &self,
        collection: &str,
        identifier: &str,
        actor: &AccessContext,
    ) -> Result<ReadDeletedDocument, EngineError> {
        self.get_deleted_with_access(collection, identifier, Some(actor))
    }

    fn get_deleted_with_access(
        &self,
        collection: &str,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<ReadDeletedDocument, EngineError> {
        let collection = self
            .root
            .collection(collection)
            .map_err(EngineError::storage)?;
        Self::read_stable(&collection, || {
            let stored = collection
                .read_deleted_document(identifier)
                .map_err(EngineError::storage)?;
            require_read_access(stored.access, actor)?;
            let document = self.decode_document(
                collection.name(),
                Document::parse(&stored.bytes, DocumentLimits::default())
                    .map_err(EngineError::format)?,
            )?;
            let timestamps = TtidTimestamps::decode(identifier).map_err(EngineError::format)?;
            Ok(ReadDeletedDocument {
                id: identifier.to_owned(),
                created_at: timestamps.created_at,
                deleted_at: stored.modified_millis,
                document,
            })
        })
    }

    /// Read one retained soft-deleted raw file.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe/corrupt storage, invalid identifiers,
    /// or concurrent write generations.
    pub fn get_deleted_file(
        &self,
        collection: &str,
        identifier: &str,
    ) -> Result<ReadDeletedFile, EngineError> {
        self.get_deleted_file_with_access(collection, identifier, None)
    }

    /// Read one retained deleted raw file as a trusted actor.
    ///
    /// # Errors
    ///
    /// Returns `EACCES` when the retained descriptor denies reads.
    pub fn get_deleted_file_as(
        &self,
        collection: &str,
        identifier: &str,
        actor: &AccessContext,
    ) -> Result<ReadDeletedFile, EngineError> {
        self.get_deleted_file_with_access(collection, identifier, Some(actor))
    }

    fn get_deleted_file_with_access(
        &self,
        collection: &str,
        identifier: &str,
        actor: Option<&AccessContext>,
    ) -> Result<ReadDeletedFile, EngineError> {
        let collection = self
            .root
            .collection(collection)
            .map_err(EngineError::storage)?;
        Self::read_stable(&collection, || {
            let stored = collection
                .read_deleted_raw_file(identifier)
                .map_err(EngineError::storage)?;
            require_read_access(stored.access_descriptor, actor)?;
            let deleted_at = stored.modified_millis;
            Ok(ReadDeletedFile {
                deleted_at,
                file: build_read_file(identifier, stored)?,
            })
        })
    }

    /// Execute one portable prefix-index scan under a stable generation.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe/corrupt storage, invalid queries, or a
    /// concurrent write generation.
    pub fn scan_index(
        &self,
        collection: &str,
        queries: &[ScanQuery],
    ) -> Result<Vec<String>, EngineError> {
        let collection = self
            .root
            .collection(collection)
            .map_err(EngineError::storage)?;
        Self::read_stable(&collection, || {
            collection
                .shared_index_snapshot()
                .map_err(EngineError::storage)?
                .scan(queries, QueryLimits::default())
                .map_err(EngineError::query)?
                .into_iter()
                .map(|identifier| {
                    String::from_utf8(identifier).map_err(|error| {
                        EngineError::new(
                            EngineErrorCode::CorruptData,
                            format!("index document identifier is not UTF-8: {error}"),
                        )
                    })
                })
                .collect()
        })
    }

    /// Verify merged snapshot/WAL key structure and live-record references.
    ///
    /// # Errors
    ///
    /// Returns a stable error for corrupt/orphaned keys, unsafe storage, or a
    /// concurrent write generation.
    pub fn verify_index(&self, collection: &str) -> Result<IndexVerification, EngineError> {
        let collection = self
            .root
            .collection(collection)
            .map_err(EngineError::storage)?;
        Self::read_stable(&collection, || {
            let mut verification = collection
                .verify_index_references()
                .map_err(EngineError::storage)?;
            let encrypted_fields = self
                .encryption
                .as_ref()
                .map(|encryption| encryption.encrypted_fields(collection.name()))
                .transpose()
                .map_err(EngineError::encryption)?
                .unwrap_or_default();
            let mut expected = BTreeSet::new();
            let identifiers = match collection.kind() {
                CollectionKind::Document => {
                    collection.document_ids().map_err(EngineError::storage)?
                }
                CollectionKind::File => collection.raw_file_ids().map_err(EngineError::storage)?,
            };
            for identifier in identifiers {
                let fields = match collection.kind() {
                    CollectionKind::Document => {
                        let stored = collection
                            .read_document(&identifier)
                            .map_err(EngineError::storage)?;
                        // The index is built from documents as stored, so it
                        // is checked against them, not their head shape.
                        self.decrypt_fields(
                            collection.name(),
                            Document::parse(&stored.bytes, DocumentLimits::default())
                                .map_err(EngineError::format)?
                                .into_fields(),
                            None,
                        )?
                    }
                    CollectionKind::File => {
                        let stored = collection
                            .read_raw_file(&identifier)
                            .map_err(EngineError::storage)?;
                        raw_file_index_fields(&identifier, stored)?
                    }
                };
                expected.extend(
                    index_entries_for_document(&identifier, &fields, |field, value| {
                        if is_encrypted_field(field, &encrypted_fields) {
                            let encryption = self.encryption.as_ref().ok_or_else(|| {
                                "encrypted index field has no schema/key context".to_owned()
                            })?;
                            encryption
                                .blind_index(value)
                                .map(IndexLookupValue::encrypted)
                        } else {
                            Ok(IndexLookupValue::plain(value))
                        }
                    })
                    .map_err(EngineError::encryption)?,
                );
            }
            let snapshot = collection.index_snapshot().map_err(EngineError::storage)?;
            let actual: BTreeSet<String> = snapshot
                .as_bytes()
                .split(|byte| *byte == b'\n')
                .filter(|key| !key.is_empty())
                .map(|key| {
                    std::str::from_utf8(key)
                        .map(str::to_owned)
                        .map_err(|error| {
                            EngineError::new(
                                EngineErrorCode::Storage,
                                format!("prefix index key is not valid UTF-8: {error}"),
                            )
                        })
                })
                .collect::<Result<_, _>>()?;
            verification.expected_key_count = Some(expected.len());
            verification.missing_keys = Some(expected.difference(&actual).count());
            verification.extra_keys = Some(actual.difference(&expected).count());
            verification.rebuild_equivalent = actual == expected;
            // A count alone cannot be acted on: an operator, and a failing CI
            // job, need to see which keys drifted. The sample is bounded so a
            // wholesale mismatch cannot produce an unbounded report.
            verification.missing_key_sample = expected
                .difference(&actual)
                .take(MAX_DRIFT_SAMPLE)
                .cloned()
                .collect();
            verification.extra_key_sample = actual
                .difference(&expected)
                .take(MAX_DRIFT_SAMPLE)
                .cloned()
                .collect();
            Ok(verification)
        })
    }

    /// Execute a validated structured query over live JSON documents.
    ///
    /// Candidate planning remains a separate optimization; this bounded
    /// read-only path evaluates portable predicates in deterministic TTID
    /// order and preserves the existing zero-limit behavior.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe/corrupt storage, document format
    /// failures, or a concurrent write generation.
    pub fn find(
        &self,
        collection: &str,
        query: &StructuredQuery,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        self.find_with_access(collection, query, None)
    }

    fn plan_equality_candidates(
        &self,
        collection: &NativeCollection,
        query: &StructuredQuery,
    ) -> Result<Option<Vec<String>>, EngineError> {
        let encrypted_fields = self.encrypted_fields(collection.name())?;
        let Some(plan) = equality_plan::EqualityPlan::new(query, &encrypted_fields) else {
            return Ok(None);
        };
        // An index built from documents below the head can be wrong for the
        // fields their migrations write; such a query reads every document.
        if let Some(guard) = self.migration_guard(collection)?
            && !guard.allows(&plan)
        {
            return Ok(None);
        }
        let snapshot = collection
            .shared_index_snapshot()
            .map_err(EngineError::storage)?;
        plan.scan(&snapshot).map(Some)
    }

    fn encrypted_fields(&self, collection: &str) -> Result<Vec<String>, EngineError> {
        Ok(self
            .encryption
            .as_ref()
            .map(|encryption| encryption.encrypted_fields(collection))
            .transpose()
            .map_err(EngineError::encryption)?
            .unwrap_or_default())
    }

    /// Join two document collections.
    ///
    /// A nested loop over both sides, matching the JavaScript engine: joins are
    /// answered from documents rather than from the prefix index, because the
    /// index records keys and a join compares values.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe/corrupt storage or an invalid join.
    pub fn join(
        &self,
        join: &JoinSpec,
        actor: Option<&AccessContext>,
    ) -> Result<JoinResult, EngineError> {
        let empty = StructuredQuery::from_value(&json!({}), QueryLimits::default())
            .map_err(EngineError::query)?;
        let left = self.find_with_access(join.left_collection(), &empty, actor)?;
        let right = self.find_with_access(join.right_collection(), &empty, actor)?;

        let mut rows: Vec<JoinedRow> = Vec::new();
        'outer: for left_row in &left {
            for right_row in &right {
                if !join.matches(left_row.document.fields(), right_row.document.fields()) {
                    continue;
                }
                // The pair identifier is the JavaScript key verbatim: two
                // identifiers separated by a comma and a space.
                rows.push((
                    format!("{}, {}", left_row.metadata.id, right_row.metadata.id),
                    join.project(left_row.document.fields(), right_row.document.fields()),
                ));
                if join.limit().is_some_and(|limit| rows.len() >= limit) {
                    break 'outer;
                }
            }
        }

        let Some(field) = join.group_by() else {
            if join.only_ids() {
                return Ok(JoinResult::Ids(
                    rows.into_iter().map(|(id, _)| id).collect(),
                ));
            }
            return Ok(JoinResult::Rows(
                rows.into_iter()
                    .map(|(id, row)| (id, Value::Object(row)))
                    .collect(),
            ));
        };
        let mut grouped: BTreeMap<String, Vec<JoinedRow>> = BTreeMap::new();
        for (id, row) in rows {
            // `String(data[field])` in JavaScript, so an absent field buckets
            // under "undefined" rather than being dropped.
            let key = match row.get(field) {
                None => "undefined".to_owned(),
                Some(Value::String(text)) => text.clone(),
                Some(value) => value.to_string(),
            };
            grouped.entry(key).or_default().push((id, row));
        }
        if join.only_ids() {
            return Ok(JoinResult::GroupedIds(
                grouped
                    .into_iter()
                    .map(|(key, rows)| (key, rows.into_iter().map(|(id, _)| id).collect()))
                    .collect(),
            ));
        }
        Ok(JoinResult::Grouped(
            grouped
                .into_iter()
                .map(|(key, rows)| {
                    (
                        key,
                        rows.into_iter()
                            .map(|(id, row)| (id, Value::Object(row)))
                            .collect(),
                    )
                })
                .collect(),
        ))
    }

    /// Execute a structured query as a trusted actor.
    ///
    /// Protected rows that deny reads are omitted, matching the JavaScript
    /// collection cursor.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe/corrupt storage or invalid queries.
    pub fn find_as(
        &self,
        collection: &str,
        query: &StructuredQuery,
        actor: &AccessContext,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        self.find_with_access(collection, query, Some(actor))
    }

    /// Execute a structured query over live raw-file manifests.
    ///
    /// # Errors
    ///
    /// Returns a stable error for a non-file collection, unsafe/corrupt
    /// storage, invalid queries, or an unstable generation.
    pub fn find_files(
        &self,
        collection: &str,
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
    ) -> Result<Vec<RawFileQueryRow>, EngineError> {
        let collection = self
            .root
            .collection(collection)
            .map_err(EngineError::storage)?;
        if collection.kind() != CollectionKind::File {
            return Err(EngineError::new(
                EngineErrorCode::Storage,
                "raw-file query requires a file collection",
            ));
        }
        Self::read_stable(&collection, || {
            let identifiers = match self.plan_equality_candidates(&collection, query)? {
                Some(candidates) => candidates,
                None => collection.raw_file_ids().map_err(EngineError::storage)?,
            };
            let mut records = Vec::new();
            for identifier in identifiers {
                let stored = collection
                    .read_raw_file(&identifier)
                    .map_err(EngineError::storage)?;
                if !read_access_allowed(stored.access_descriptor, actor) {
                    continue;
                }
                let updated_at = stored.modified_millis;
                let fields = raw_file_index_fields(&identifier, stored)?;
                let created_at = TtidTimestamps::decode(&identifier)
                    .map_err(EngineError::format)?
                    .created_at;
                if !query.matches(&fields, created_at, updated_at) {
                    continue;
                }
                records.push((identifier, fields));
                if query
                    .limit()
                    .is_some_and(|limit| limit > 0 && records.len() >= limit)
                {
                    break;
                }
            }
            Ok(records)
        })
    }

    /// Execute a structured query over retained raw-file manifests.
    ///
    /// # Errors
    ///
    /// Returns a stable error for a non-file collection, unsafe/corrupt
    /// storage, invalid queries, or an unstable generation.
    pub fn find_deleted_files(
        &self,
        collection: &str,
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
    ) -> Result<Vec<RawFileQueryRow>, EngineError> {
        let collection = self
            .root
            .collection(collection)
            .map_err(EngineError::storage)?;
        if collection.kind() != CollectionKind::File {
            return Err(EngineError::new(
                EngineErrorCode::Storage,
                "deleted raw-file query requires a file collection",
            ));
        }
        Self::read_stable(&collection, || {
            let mut records = Vec::new();
            for identifier in collection
                .deleted_raw_file_ids()
                .map_err(EngineError::storage)?
            {
                let stored = collection
                    .read_deleted_raw_file(&identifier)
                    .map_err(EngineError::storage)?;
                if !read_access_allowed(stored.access_descriptor, actor) {
                    continue;
                }
                let deleted_at = stored.modified_millis;
                let fields = raw_file_index_fields(&identifier, stored)?;
                let created_at = TtidTimestamps::decode(&identifier)
                    .map_err(EngineError::format)?
                    .created_at;
                if !query.matches(&fields, created_at, deleted_at) {
                    continue;
                }
                records.push((identifier, fields));
                if query
                    .limit()
                    .is_some_and(|limit| limit > 0 && records.len() >= limit)
                {
                    break;
                }
            }
            Ok(records)
        })
    }

    fn find_with_access(
        &self,
        collection: &str,
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        match &self.hot_set {
            Some(hot_set) => HotReader::new(self, hot_set, collection).find(query, actor),
            None => self.find_from_storage(collection, query, actor),
        }
    }

    fn find_from_storage(
        &self,
        collection: &str,
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
    ) -> Result<Vec<ReadDocument>, EngineError> {
        let collection = self.open_collection(collection)?;
        Self::read_stable(&collection, || {
            // Documents are the truth and the index is an accelerator, so a
            // planned query narrows which documents are read and the predicate
            // is still applied to each one. The index can only over-match — an
            // array is indexed per element — never under-match.
            let planned = self.plan_equality_candidates(&collection, query)?;
            let identifiers = match planned {
                Some(candidates) => candidates,
                None => collection.document_ids().map_err(EngineError::storage)?,
            };
            let mut records = Vec::new();
            for batch in identifiers.chunks(Self::read_batch(&collection)) {
                let stored_batch = collection
                    .read_documents(batch)
                    .map_err(EngineError::storage)?;
                for (identifier, stored) in batch.iter().zip(stored_batch) {
                    if let Some(record) =
                        self.query_record(&collection, query, actor, (identifier, stored))?
                    {
                        records.push(record);
                    }
                    if query
                        .limit()
                        .is_some_and(|limit| limit > 0 && records.len() >= limit)
                    {
                        return Ok(records);
                    }
                }
            }
            Ok(records)
        })
    }

    /// Documents a query reads at a time. A segment collection reads a batch
    /// with a few coalesced reads; a file collection reads one file per
    /// document either way, so it reads one at a time and stops at a limit
    /// without reading further.
    fn read_batch(collection: &NativeCollection) -> usize {
        match collection.layout() {
            CollectionLayout::Segments => Self::SEGMENT_READ_BATCH,
            CollectionLayout::Files => 1,
        }
    }

    /// The query result for one stored document, or `None` when the actor
    /// may not read it or the predicate does not match.
    fn query_record(
        &self,
        collection: &NativeCollection,
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
        (identifier, stored): (&str, StoredBytes),
    ) -> Result<Option<ReadDocument>, EngineError> {
        if !read_access_allowed(stored.access, actor) {
            return Ok(None);
        }
        let record = self.build_record(collection.name(), identifier, &stored)?;
        if !query.matches(
            record.document.fields(),
            record.metadata.created_at,
            record.metadata.updated_at,
        ) {
            return Ok(None);
        }
        Ok(Some(record))
    }

    /// Execute a structured query across retained tombstones.
    ///
    /// Ordering and predicate semantics match the live cursor; only the source
    /// namespace differs.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe/corrupt storage or invalid queries.
    pub fn find_deleted(
        &self,
        collection: &str,
        query: &StructuredQuery,
        actor: Option<&AccessContext>,
    ) -> Result<Vec<ReadDeletedDocument>, EngineError> {
        let collection = self
            .root
            .collection(collection)
            .map_err(EngineError::storage)?;
        Self::read_stable(&collection, || {
            let mut records = Vec::new();
            for identifier in collection
                .deleted_document_ids()
                .map_err(EngineError::storage)?
            {
                let stored = collection
                    .read_deleted_document(&identifier)
                    .map_err(EngineError::storage)?;
                if !read_access_allowed(stored.access, actor) {
                    continue;
                }
                let document = self.decode_document(
                    collection.name(),
                    Document::parse(&stored.bytes, DocumentLimits::default())
                        .map_err(EngineError::format)?,
                )?;
                let timestamps =
                    TtidTimestamps::decode(&identifier).map_err(EngineError::format)?;
                if !query.matches(
                    document.fields(),
                    timestamps.created_at,
                    stored.modified_millis,
                ) {
                    continue;
                }
                records.push(ReadDeletedDocument {
                    id: identifier,
                    created_at: timestamps.created_at,
                    deleted_at: stored.modified_millis,
                    document,
                });
                if query
                    .limit()
                    .is_some_and(|limit| limit > 0 && records.len() >= limit)
                {
                    break;
                }
            }
            Ok(records)
        })
    }

    /// Execute a prepared read-only SQL `SELECT` plan.
    ///
    /// Projection, only-ID shaping, grouping, predicates, ordering, and limit
    /// behavior match the current JavaScript engine. Joins and mutations are
    /// deliberately rejected by this read-only preview.
    ///
    /// # Errors
    ///
    /// Returns a stable error for non-SELECT plans, joins, malformed query
    /// ASTs, unsafe storage, or unstable collection generations.
    pub fn select_sql(&self, plan: &SqlPlan) -> Result<Value, EngineError> {
        self.select_sql_with_access(plan, None)
    }

    /// Execute a read-only SQL plan as a trusted actor.
    ///
    /// # Errors
    ///
    /// Returns a stable error for invalid plans, storage, or access failures.
    pub fn select_sql_as(
        &self,
        plan: &SqlPlan,
        actor: &AccessContext,
    ) -> Result<Value, EngineError> {
        self.select_sql_with_access(plan, Some(actor))
    }

    fn select_sql_with_access(
        &self,
        plan: &SqlPlan,
        actor: Option<&AccessContext>,
    ) -> Result<Value, EngineError> {
        if plan.operation != SqlOperation::Select {
            return Err(EngineError::new(
                EngineErrorCode::Query,
                "read-only SQL execution accepts SELECT statements only",
            ));
        }
        if plan.ast.get("$leftCollection").is_some() {
            return Err(EngineError::new(
                EngineErrorCode::Query,
                "read-only SQL joins are not implemented",
            ));
        }
        let query = StructuredQuery::from_value(&plan.ast, QueryLimits::default())
            .map_err(EngineError::query)?;
        let records = self.find_with_access(&plan.collection, &query, actor)?;
        Ok(shape_select_results(records, &plan.ast))
    }

    /// Inspect one collection without changing any files.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe, corrupt, missing, or concurrently
    /// mutating storage.
    pub fn inspect(&self, collection: &str) -> Result<CollectionInspection, EngineError> {
        let collection = self
            .root
            .collection(collection)
            .map_err(EngineError::storage)?;
        Self::read_stable(&collection, || {
            let generation = collection.generation().map_err(EngineError::storage)?;
            let (document_count, file_count, deleted_count) = match collection.kind() {
                CollectionKind::Document => (
                    collection
                        .document_ids()
                        .map_err(EngineError::storage)?
                        .len(),
                    0,
                    collection
                        .deleted_document_ids()
                        .map_err(EngineError::storage)?
                        .len(),
                ),
                CollectionKind::File => (
                    0,
                    collection
                        .raw_file_ids()
                        .map_err(EngineError::storage)?
                        .len(),
                    collection
                        .deleted_raw_file_ids()
                        .map_err(EngineError::storage)?
                        .len(),
                ),
            };
            let index_bytes = collection
                .index_snapshot()
                .map_err(EngineError::storage)?
                .as_bytes()
                .len();
            Ok(CollectionInspection {
                collection: collection.name().to_owned(),
                kind: collection.kind(),
                layout: collection.layout(),
                generation: generation.generation,
                state: generation.state,
                document_count,
                file_count,
                deleted_count,
                index_bytes,
                validation: collection.validation(),
                schema_violations: fylo_storage_native::schema_violations(
                    self.root.path(),
                    collection.name(),
                ),
                read_only: true,
            })
        })
    }

    fn read_stable<T>(
        collection: &NativeCollection,
        read: impl Fn() -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        for _ in 0..MAX_STABLE_READ_ATTEMPTS {
            let Some(before) = Self::read_attempt_generation(collection)? else {
                continue;
            };
            let value = read();
            let Some(after) = Self::read_attempt_generation(collection)? else {
                continue;
            };
            if stable_generation_observed(after.state, before.generation, after.generation) {
                return value;
            }
        }
        Err(Self::concurrent_write())
    }

    /// The collection's generation and log end, refusing a collection that
    /// is being written or has a committed record not yet applied.
    fn stable_generation(collection: &NativeCollection) -> Result<GenerationState, EngineError> {
        Self::read_attempt_generation(collection)?.ok_or_else(Self::concurrent_write)
    }

    fn concurrent_write() -> EngineError {
        EngineError::new(
            EngineErrorCode::ConcurrentWrite,
            "collection changed during a read-only operation",
        )
    }

    fn access_denied() -> EngineError {
        EngineError::new(
            EngineErrorCode::Access,
            "portable FYLO access descriptor denied the read",
        )
    }

    fn read_attempt_generation(
        collection: &NativeCollection,
    ) -> Result<Option<GenerationState>, EngineError> {
        match collection.generation() {
            Ok(state) if state.state == GenerationStatus::Writing => Ok(None),
            Ok(state) => Ok(Some(state)),
            Err(error) if error.code() == NativeStorageErrorCode::ConcurrentWrite => Ok(None),
            Err(error) => Err(EngineError::storage(error)),
        }
    }

    /// The one point every document read passes: decrypt the stored body
    /// and, for a document below its collection's head version, migrate it
    /// to the head in memory (ADR 0017 section 5).
    fn decode_document(
        &self,
        collection: &str,
        document: Document,
    ) -> Result<Document, EngineError> {
        let fields = self.present_fields(collection, document.into_fields())?;
        Document::try_from_value(Value::Object(fields), DocumentLimits::default())
            .map_err(EngineError::format)
    }
}

fn require_read_access(
    descriptor: Option<AccessDescriptor>,
    actor: Option<&AccessContext>,
) -> Result<(), EngineError> {
    if read_access_allowed(descriptor, actor) {
        Ok(())
    } else {
        Err(ReadOnlyEngine::access_denied())
    }
}

fn read_access_allowed(
    descriptor: Option<AccessDescriptor>,
    actor: Option<&AccessContext>,
) -> bool {
    read_access_conditions(descriptor, actor)
        .into_iter()
        .any(|condition| condition)
}

fn read_access_conditions(
    descriptor: Option<AccessDescriptor>,
    actor: Option<&AccessContext>,
) -> [bool; 4] {
    let Some(descriptor) = descriptor else {
        return [true, false, false, false];
    };
    let owner = actor.is_some_and(|actor| actor.uid == descriptor.uid);
    let group = !owner && actor.is_some_and(|actor| actor.groups.contains(&descriptor.gid));
    let other = !owner && !group;
    [
        false,
        owner && ((descriptor.mode >> 6) & 0o4 != 0),
        group && ((descriptor.mode >> 3) & 0o4 != 0),
        other && (descriptor.mode & 0o4 != 0),
    ]
}

fn stable_generation_observed(
    after_state: GenerationStatus,
    before_generation: u64,
    after_generation: u64,
) -> bool {
    after_state == GenerationStatus::Stable && after_generation == before_generation
}

fn build_read_file(identifier: &str, stored: StoredRawFile) -> Result<ReadFile, EngineError> {
    let timestamps = TtidTimestamps::decode(identifier).map_err(EngineError::format)?;
    let metadata = CanonicalMetadata {
        id: identifier.to_owned(),
        created_at: timestamps.created_at,
        updated_at: stored.modified_millis,
        mtime: stored.modified_millis,
    };
    Ok(ReadFile {
        file: RawFileManifest {
            name: format!("{identifier}{}", stored.extension),
            key: stored.key,
            extension: stored.extension,
            content_type: stored.content_type,
            content_length: stored.bytes.len() as u64,
            etag: stored.checksum_sha256.clone(),
            checksum_sha256: stored.checksum_sha256,
            created_at: timestamps.created_at,
            last_modified: stored.modified_millis,
        },
        metadata,
        custom_metadata: stored.custom_metadata.into_iter().collect(),
        access: stored.access,
        bytes: stored.bytes,
    })
}

fn raw_file_index_fields(
    identifier: &str,
    stored: StoredRawFile,
) -> Result<Map<String, Value>, EngineError> {
    let custom_metadata = stored.custom_metadata.clone();
    let last_modified = stored.modified_millis;
    let manifest = build_read_file(identifier, stored)?.file;
    let Value::Object(mut fields) = serde_json::to_value(manifest).map_err(|error| {
        EngineError::new(
            EngineErrorCode::Storage,
            format!("cannot encode raw-file index manifest: {error}"),
        )
    })?
    else {
        unreachable!("RawFileManifest serializes as an object");
    };
    fields.insert("lastModified".into(), Value::from(last_modified));
    if !custom_metadata.is_empty() {
        fields.insert(
            "meta".into(),
            Value::Object(custom_metadata.into_iter().collect()),
        );
    }
    Ok(fields)
}

fn shape_select_results(records: Vec<ReadDocument>, ast: &Value) -> Value {
    let selection = ast
        .get("$select")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let only_ids = ast
        .get("$onlyIds")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let group_by = ast.get("$groupby").and_then(Value::as_str);
    if let Some(group_by) = group_by {
        return shape_grouped_results(records, &selection, group_by, only_ids);
    }
    if only_ids {
        return Value::Array(
            records
                .into_iter()
                .map(|record| Value::String(record.metadata.id))
                .collect(),
        );
    }
    Value::Object(
        records
            .into_iter()
            .map(|record| {
                (
                    record.metadata.id,
                    Value::Object(project(record.document.into_fields(), &selection)),
                )
            })
            .collect(),
    )
}

fn shape_grouped_results(
    records: Vec<ReadDocument>,
    selection: &[&str],
    group_by: &str,
    only_ids: bool,
) -> Value {
    let mut groups = Map::new();
    for record in records {
        let mut fields = project(record.document.into_fields(), selection);
        let Some(group_value) = fields.get(group_by).filter(|value| js_truthy(value)) else {
            continue;
        };
        let group = js_string(group_value);
        fields.remove(group_by);
        let members = groups
            .entry(group)
            .or_insert_with(|| Value::Object(Map::new()));
        members
            .as_object_mut()
            .expect("group entries are created as objects")
            .insert(record.metadata.id, Value::Object(fields));
    }
    if !only_ids {
        return Value::Object(groups);
    }
    Value::Object(
        groups
            .into_iter()
            .map(|(group, members)| {
                let identifiers = members
                    .as_object()
                    .expect("group entries are objects")
                    .keys()
                    .cloned()
                    .map(Value::String)
                    .collect();
                (group, Value::Array(identifiers))
            })
            .collect(),
    )
}

fn project(mut fields: Map<String, Value>, selection: &[&str]) -> Map<String, Value> {
    if !selection.is_empty() {
        fields.retain(|field, _| selection.contains(&field.as_str()));
    }
    fields
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// Validated document plus canonical storage metadata.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadDocument {
    /// Canonical metadata.
    pub metadata: CanonicalMetadata,
    /// Stored JSON document.
    pub document: Document,
}

/// One joined pair: the `"<leftId>, <rightId>"` key and the projected row.
type JoinedRow = (String, Map<String, Value>);

/// One raw-file query result: its TTID and public manifest fields.
pub type RawFileQueryRow = (String, Map<String, Value>);

/// One join's result, in the four shapes the JavaScript engine returns.
///
/// The shape is chosen by the join, not by the caller: `$groupby` buckets the
/// rows and `$onlyIds` drops the bodies, so the four combinations are distinct
/// result types rather than one type with empty fields.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum JoinResult {
    /// Joined rows keyed by `"<leftId>, <rightId>"`.
    Rows(Map<String, Value>),
    /// Joined pair identifiers only.
    Ids(Vec<String>),
    /// Rows bucketed by the `$groupby` field.
    Grouped(BTreeMap<String, Map<String, Value>>),
    /// Pair identifiers bucketed by the `$groupby` field.
    GroupedIds(BTreeMap<String, Vec<String>>),
}

/// Retained soft-deleted JSON document.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadDeletedDocument {
    /// Original TTID.
    pub id: String,
    /// TTID creation timestamp.
    pub created_at: u64,
    /// Tombstone modification/deletion timestamp in whole Unix milliseconds.
    pub deleted_at: u64,
    /// Stored JSON document.
    pub document: Document,
}

/// Raw-file bytes plus all metadata exposed by the read-only engine.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFile {
    /// Canonical TTID and filesystem timestamps.
    pub metadata: CanonicalMetadata,
    /// Existing FYLO raw-file manifest fields.
    pub file: RawFileManifest,
    /// Developer-defined xattr/ADS metadata.
    pub custom_metadata: Map<String, Value>,
    /// Native owner, group, and permission mode.
    pub access: NativeAccess,
    /// Unwrapped file bytes. Protocol adapters must choose a bounded encoding.
    #[serde(skip_serializing)]
    pub bytes: Vec<u8>,
}

/// Retained soft-deleted raw file.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadDeletedFile {
    /// Tombstone modification/deletion timestamp in whole Unix milliseconds.
    pub deleted_at: u64,
    /// Raw-file body and metadata.
    #[serde(flatten)]
    pub file: ReadFile,
}

/// Existing FYLO raw-file manifest representation.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawFileManifest {
    /// TTID filename.
    pub name: String,
    /// Durable logical object key.
    pub key: String,
    /// Safe final extension.
    pub extension: String,
    /// Inferred content type.
    pub content_type: String,
    /// Unwrapped byte length.
    pub content_length: u64,
    /// Entity tag, equal to the cached/recomputed SHA-256.
    pub etag: String,
    /// SHA-256 checksum.
    #[serde(rename = "checksumSHA256")]
    pub checksum_sha256: String,
    /// TTID creation time in Unix milliseconds.
    pub created_at: u64,
    /// Filesystem modification time in whole Unix milliseconds.
    pub last_modified: u64,
}

/// Read-only collection inspection result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionInspection {
    /// Collection name.
    pub collection: String,
    /// Document or file collection.
    pub kind: CollectionKind,
    /// How the collection stores its documents.
    pub layout: CollectionLayout,
    /// Stable transaction generation.
    pub generation: u64,
    /// Stable/writing state observed by the reader.
    pub state: GenerationStatus,
    /// Validated live JSON document count.
    pub document_count: usize,
    /// Validated live raw-file count.
    pub file_count: usize,
    /// Validated retained tombstone count.
    pub deleted_count: usize,
    /// Immutable index snapshot bytes, or zero when absent.
    pub index_bytes: usize,
    /// Validation mode the collection's descriptor records. `None` means the
    /// writing host's `strict_schema` setting decides (ADR 0017 section 2).
    pub validation: Option<ValidationMode>,
    /// Writes this process committed in `warn` mode although the schema
    /// rejected them.
    pub schema_violations: u64,
    /// Always true for this preview engine.
    pub read_only: bool,
}

/// Stable read-only engine error codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineErrorCode {
    /// Native storage rejected the operation.
    Storage,
    /// Portable format validation failed.
    Format,
    /// Portable query validation/execution failed.
    Query,
    /// Index or identifier data was corrupt.
    CorruptData,
    /// A writer prevented a stable generation read.
    ConcurrentWrite,
    /// Encrypted data could not be safely decoded.
    Encryption,
    /// Portable UID/GID/mode policy denied the operation.
    Access,
    /// The native engine cannot honour a documented contract on this input.
    Unsupported,
    /// A document failed schema validation, or schema tooling failed.
    Schema,
    /// A schema version label is already committed with different content;
    /// committed versions are immutable (ADR 0017 section 3).
    SchemaConflict,
    /// A schema declares an array whose items are objects, which the
    /// document format refuses on every write (ADR 0017 section 2).
    ArrayOfObjects,
    /// This build cannot validate the collection's head schema version, so
    /// it refuses the write rather than store an unvalidated document
    /// (ADR 0017 section 4, "Fail closed").
    SchemaUnavailable,
}

impl EngineErrorCode {
    /// Stable external string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "EENGINE_STORAGE",
            Self::Format => "EENGINE_FORMAT",
            Self::Query => "EENGINE_QUERY",
            Self::CorruptData => "EENGINE_CORRUPT",
            Self::ConcurrentWrite => "EENGINE_CONCURRENT_WRITE",
            Self::Encryption => "EENGINE_ENCRYPTION",
            Self::Access => "EACCES",
            Self::Unsupported => "EENGINE_UNSUPPORTED",
            Self::Schema => "ESCHEMA",
            Self::SchemaConflict => "ESCHEMACONFLICT",
            Self::ArrayOfObjects => "EARRAYOFOBJECTS",
            Self::SchemaUnavailable => "ESCHEMAUNAVAILABLE",
        }
    }
}

/// Read-only engine error.
#[derive(Debug)]
pub struct EngineError {
    code: EngineErrorCode,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl EngineError {
    fn new(code: EngineErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            source: None,
        }
    }

    fn schema(message: String) -> Self {
        Self::new(EngineErrorCode::Schema, message)
    }

    #[cfg(any(test, target_arch = "wasm32"))]
    fn schema_unavailable(message: String) -> Self {
        Self::new(EngineErrorCode::SchemaUnavailable, message)
    }

    fn storage(error: NativeStorageError) -> Self {
        // Storage enforces the schema (ADR 0017 section 1), but a rejected
        // document is the same schema failure it was when the engine refused
        // it, with the same code and message.
        if error.code() == NativeStorageErrorCode::SchemaViolation {
            return Self::schema(error.message().to_owned());
        }
        Self {
            code: EngineErrorCode::Storage,
            message: error.to_string(),
            source: Some(Box::new(error)),
        }
    }

    /// Native storage code when this error originated below the engine.
    #[must_use]
    pub fn storage_code(&self) -> Option<NativeStorageErrorCode> {
        self.source
            .as_deref()
            .and_then(|source| source.downcast_ref::<NativeStorageError>())
            .map(NativeStorageError::code)
    }

    /// Portable format code when this error originated in the canonical codec.
    #[must_use]
    pub fn format_code(&self) -> Option<FormatErrorCode> {
        self.source
            .as_deref()
            .and_then(|source| source.downcast_ref::<FormatError>())
            .map(FormatError::code)
    }

    /// Stable external format code when this error originated in the codec.
    #[must_use]
    pub fn format_code_str(&self) -> Option<&'static str> {
        self.format_code().map(FormatErrorCode::as_str)
    }

    /// Portable query code when this error originated in the query kernel.
    #[must_use]
    pub fn query_code(&self) -> Option<QueryErrorCode> {
        self.source
            .as_deref()
            .and_then(|source| source.downcast_ref::<QueryError>())
            .map(QueryError::code)
    }

    /// Stable external query code when this error originated in the query kernel.
    #[must_use]
    pub fn query_code_str(&self) -> Option<&'static str> {
        self.query_code().map(QueryErrorCode::as_str)
    }

    fn format(error: FormatError) -> Self {
        Self {
            code: EngineErrorCode::Format,
            message: error.to_string(),
            source: Some(Box::new(error)),
        }
    }

    fn query(error: QueryError) -> Self {
        Self {
            code: EngineErrorCode::Query,
            message: error.to_string(),
            source: Some(Box::new(error)),
        }
    }

    fn encryption(message: impl Into<String>) -> Self {
        Self::new(EngineErrorCode::Encryption, message)
    }

    /// Stable error code.
    #[must_use]
    pub const fn code(&self) -> EngineErrorCode {
        self.code
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|error| error.as_ref() as &(dyn std::error::Error + 'static))
    }
}

#[cfg(test)]
mod mcdc_tests {
    use super::*;
    use fylo_test_support::{McdcCase, assert_unique_cause_mcdc};

    fn access_case(
        name: &'static str,
        descriptor: Option<AccessDescriptor>,
        actor: Option<&AccessContext>,
    ) -> McdcCase<4> {
        McdcCase::new(
            name,
            read_access_conditions(descriptor, actor),
            read_access_allowed(descriptor, actor),
        )
    }

    #[test]
    fn mcdc_engine_read_access_policy() {
        let protected = AccessDescriptor {
            version: 1,
            uid: 1_001,
            gid: 2_001,
            mode: 0,
        };
        let owner = AccessContext::new(1_001, [2_001]);
        let group_member = AccessContext::new(1_002, [2_001]);
        let outsider = AccessContext::new(1_003, []);
        let cases = [
            access_case("deny_baseline", Some(protected), Some(&outsider)),
            access_case("unprotected", None, Some(&outsider)),
            access_case(
                "owner_read",
                Some(AccessDescriptor {
                    mode: 0o400,
                    ..protected
                }),
                Some(&owner),
            ),
            access_case(
                "group_read",
                Some(AccessDescriptor {
                    mode: 0o040,
                    ..protected
                }),
                Some(&group_member),
            ),
            access_case(
                "other_read",
                Some(AccessDescriptor {
                    mode: 0o004,
                    ..protected
                }),
                Some(&outsider),
            ),
        ];
        assert_unique_cause_mcdc(
            "engine.read_access_policy",
            ["unprotected", "owner_read", "group_read", "other_read"],
            &cases,
        );
    }

    fn stable_case(
        name: &'static str,
        state: GenerationStatus,
        before: u64,
        after: u64,
    ) -> McdcCase<2> {
        McdcCase::new(
            name,
            [state == GenerationStatus::Stable, after == before],
            stable_generation_observed(state, before, after),
        )
    }

    #[test]
    fn mcdc_engine_stable_generation_observation() {
        let cases = [
            stable_case("stable_same_generation", GenerationStatus::Stable, 7, 7),
            stable_case("writer_visible", GenerationStatus::Writing, 7, 7),
            stable_case("generation_changed", GenerationStatus::Stable, 7, 8),
        ];
        assert_unique_cause_mcdc(
            "engine.stable_generation_observation",
            ["after_is_stable", "generation_unchanged"],
            &cases,
        );
    }
}
