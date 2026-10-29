#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(any(test, target_arch = "wasm32"))]
use std::sync::OnceLock;

use serde_json::{Map, Value};

use super::registry_version::RegistryVersion;
use crate::encryption::{encrypted_list, validate_version};
use crate::schema::schema_migration::SchemaMigration;

/// A registry version parsed once and kept in memory: its compiled validator,
/// its encryption list and its migration steps.
pub(crate) struct LoadedVersion {
    pub(crate) record: RegistryVersion,
    pub(crate) encrypted: Vec<String>,
    migration: Option<SchemaMigration>,
    /// `chex` compiled by this build (ADR 0017 section 4): a native build
    /// never trusts the record's stored `compiled` bytes.
    #[cfg(not(target_arch = "wasm32"))]
    compiled: Arc<fylo_chex::CompiledSchema>,
    /// The record's stored `compiled` bytes, loaded on first use, or why they
    /// cannot be: what a build without a regex compiler validates with.
    #[cfg(any(test, target_arch = "wasm32"))]
    stored: OnceLock<Result<fylo_chex::CompiledSchema, String>>,
}

impl LoadedVersion {
    /// Parse a version file, refusing one that is not the version the
    /// registry says it is or whose `chex` no longer matches its digest.
    pub(crate) fn parse(collection: &str, ordinal: u32, bytes: &[u8]) -> Result<Self, String> {
        let record: RegistryVersion = serde_json::from_slice(bytes).map_err(|error| {
            format!("schema version {ordinal} of {collection} is corrupt: {error}")
        })?;
        let consistent = record.format == RegistryVersion::FORMAT
            && record.ordinal == ordinal
            && record.sha256 == RegistryVersion::digest(&record.chex)
            && (ordinal == 1) == record.migration.is_none();
        if !consistent {
            return Err(format!(
                "schema version {ordinal} of {collection} does not match its record"
            ));
        }
        Self::from_record(collection, record)
    }

    pub(crate) fn from_record(collection: &str, record: RegistryVersion) -> Result<Self, String> {
        // Labels name files when a registry is pulled back out as a folder.
        validate_version(&record.label)
            .map_err(|_| format!("schema version label {:?} is invalid", record.label))?;
        let encrypted = encrypted_list(collection, &record.chex)?;
        let migration = record
            .migration
            .as_ref()
            .map(SchemaMigration::parse)
            .transpose()
            .map_err(|error| {
                format!(
                    "migration to {} of {collection} is invalid: {error}",
                    record.label
                )
            })?;
        #[cfg(not(target_arch = "wasm32"))]
        let compiled = fylo_chex::CompiledSchema::compile(
            &record.chex,
            &Self::error_label(collection, &record.label),
        )
        .map(Arc::new)
        .map_err(|error| error.message)?;
        Ok(Self {
            record,
            encrypted,
            migration,
            #[cfg(not(target_arch = "wasm32"))]
            compiled,
            #[cfg(any(test, target_arch = "wasm32"))]
            stored: OnceLock::new(),
        })
    }

    /// `record` with its `compiled` and `portable` fields derived from its
    /// `chex`, as a push commits it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn compiled_record(
        collection: &str,
        record: RegistryVersion,
    ) -> Result<Self, String> {
        let mut loaded = Self::from_record(collection, record)?;
        loaded.record.compiled = Some(crate::encryption::encode_base64_url(
            &loaded.compiled.to_bytes(),
        ));
        loaded.record.portable = Some(loaded.compiled.is_portable());
        Ok(loaded)
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn compiled_record(
        _collection: &str,
        _record: RegistryVersion,
    ) -> Result<Self, String> {
        Err("schemas are compiled and pushed natively (ADR 0017 section 4)".into())
    }

    pub(crate) fn label(&self) -> &str {
        &self.record.label
    }

    pub(crate) fn ordinal(&self) -> u32 {
        self.record.ordinal
    }

    /// The patterns of this version that have no DFA form, which a build
    /// without a regex compiler cannot validate.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn non_portable_patterns(&self) -> Vec<String> {
        self.compiled
            .non_portable_patterns()
            .map(str::to_owned)
            .collect()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn non_portable_patterns(&self) -> Vec<String> {
        Vec::new()
    }

    /// Validate a document body, `_v` already removed, against this version.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn validate(
        &self,
        _collection: &str,
        body: &Map<String, Value>,
    ) -> Result<(), String> {
        self.compiled.validate(body).map_err(|error| error.message)
    }

    /// A build without a regex compiler validates with the DFAs the native
    /// push stored (ADR 0017 section 4).
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn validate(
        &self,
        collection: &str,
        body: &Map<String, Value>,
    ) -> Result<(), String> {
        self.stored_validator(collection)?
            .validate(body)
            .map_err(|error| error.message)
    }

    /// The validator a build without a regex compiler runs: this version's
    /// stored `compiled` envelope, loaded once through `fylo-chex`'s
    /// validating `from_bytes`.
    ///
    /// # Errors
    ///
    /// Returns why the version cannot be validated from its stored bytes: it
    /// is not portable, has no compiled validator, or its bytes are refused
    /// or were not compiled from its `chex`. The caller refuses the write
    /// with `ESCHEMAUNAVAILABLE` rather than store an unvalidated document.
    #[cfg(any(test, target_arch = "wasm32"))]
    pub(crate) fn stored_validator(
        &self,
        collection: &str,
    ) -> Result<&fylo_chex::CompiledSchema, String> {
        self.stored
            .get_or_init(|| Self::load_stored(collection, &self.record))
            .as_ref()
            .map_err(String::clone)
    }

    #[cfg(any(test, target_arch = "wasm32"))]
    fn load_stored(
        collection: &str,
        record: &RegistryVersion,
    ) -> Result<fylo_chex::CompiledSchema, String> {
        let name = format!("schema {} of {collection}", record.label);
        if record.portable != Some(true) {
            return Err(format!(
                "{name} is not portable: a pattern has no DFA form, so only a native build \
                 can validate it"
            ));
        }
        let Some(encoded) = record.compiled.as_deref() else {
            return Err(format!(
                "{name} has no compiled validator; push it again with a native build"
            ));
        };
        let bytes = crate::encryption::decode_base64_url(encoded)
            .map_err(|_| format!("{name} has a compiled validator that is not base64url"))?;
        let schema = fylo_chex::CompiledSchema::from_bytes(&bytes).map_err(|error| {
            format!("{name} has a compiled validator this build refuses: {error}")
        })?;
        // The DFAs cannot be checked against their patterns without a
        // compiler, but the tree they hang from can: it must be the record's
        // own `chex`, under the label native errors name it by.
        if schema.label() != Self::error_label(collection, &record.label)
            || schema.source() != &record.chex
        {
            return Err(format!(
                "{name} has a compiled validator that was not compiled from its chex"
            ));
        }
        Ok(schema)
    }

    /// Apply this version's steps to a document at the previous version.
    pub(crate) fn migrate(&self, document: &mut Map<String, Value>) -> Result<(), String> {
        match &self.migration {
            Some(migration) => migration.apply(document),
            None => Ok(()),
        }
    }

    /// The top-level fields this version's migration may write or remove.
    pub(crate) fn migrated_fields(&self) -> Vec<String> {
        self.migration
            .as_ref()
            .map(|migration| migration.written_fields().collect())
            .unwrap_or_default()
    }

    /// Validation errors name the version as `<collection>@<label>`.
    fn error_label(collection: &str, label: &str) -> String {
        format!("{collection}@{label}")
    }
}
