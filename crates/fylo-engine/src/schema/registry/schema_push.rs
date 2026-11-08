use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value};

use super::loaded_version::LoadedVersion;
use super::registry_version::{Committed, RegistryVersion};
use super::schema_registry::SchemaRegistry;
use super::schema_rules::SchemaRules;
use crate::schema::schema_manifest::SchemaManifest;
use crate::schema::schema_paths::SchemaPaths;
use crate::schema::utils::schema_files::SchemaFiles;
use crate::{EngineError, EngineErrorCode};

/// Reads the documents a dry run checks, as `(id, decoded fields)`, after
/// [`SchemaPush::sample`] chose their ids, with how many the collection has.
pub(crate) type DryRunDocuments<'a> =
    &'a mut dyn FnMut() -> Result<(usize, Vec<(String, Map<String, Value>)>), EngineError>;

/// `fylo schema push`: commit each version of a `fylo.schema` folder that the
/// registry lacks, in manifest order (ADR 0017 section 3).
pub(crate) struct SchemaPush<'a> {
    registry: &'a SchemaRegistry,
    collection: &'a str,
    paths: SchemaPaths<'a>,
}

/// What a push did.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SchemaPushReport {
    pub(crate) collection: String,
    pub(crate) schema_dir: String,
    /// Labels committed by this push, in order.
    pub(crate) pushed: Vec<PushedVersion>,
    /// Labels the registry already held with the same content.
    pub(crate) unchanged: usize,
    pub(crate) head: Option<String>,
    /// Documents the dry run migrated and validated, and how many exist.
    pub(crate) dry_run: Option<DryRunReport>,
    /// Pushed versions that a build without a regex compiler cannot
    /// validate, with the patterns that have no DFA form (ADR 0017
    /// section 4).
    pub(crate) warnings: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct PushedVersion {
    pub(crate) ordinal: u32,
    pub(crate) label: String,
    pub(crate) sha256: String,
    pub(crate) portable: bool,
}

#[derive(Serialize)]
pub(crate) struct DryRunReport {
    pub(crate) checked: usize,
    pub(crate) total: usize,
}

impl<'a> SchemaPush<'a> {
    /// Below this many documents the dry run checks them all; above it, an
    /// evenly spaced sample of this many (ADR 0017 section 5).
    pub(crate) const DRY_RUN_DOCUMENTS: usize = 10_000;
    /// Failures a refused push reports.
    const REPORTED_FAILURES: usize = 5;

    pub(crate) fn new(
        registry: &'a SchemaRegistry,
        schema_dir: &'a Path,
        collection: &'a str,
    ) -> Self {
        Self {
            registry,
            collection,
            paths: SchemaPaths::new(schema_dir, collection),
        }
    }

    /// Push every pending version. `documents` feeds the dry run; `None`
    /// skips it, which only the one-time import does, because it adopts the
    /// schemas the documents were already written under.
    pub(crate) fn run(
        &self,
        actor: &str,
        documents: Option<DryRunDocuments<'_>>,
    ) -> Result<SchemaPushReport, EngineError> {
        let committed = self
            .registry
            .versions(self.collection)
            .map_err(EngineError::schema)?;
        let (pending, unchanged) = self.pending(&committed, actor)?;
        let mut report = SchemaPushReport {
            collection: self.collection.to_owned(),
            schema_dir: self.paths.root.display().to_string(),
            pushed: Vec::new(),
            unchanged,
            head: committed.last().map(|version| version.label().to_owned()),
            dry_run: None,
            warnings: Vec::new(),
        };
        let mut documents = documents;
        for index in 0..pending.len() {
            let version = &pending[index];
            let mut failure = None;
            let mut verify = || {
                // The first commit dry-runs every pending version, so a push
                // that would fail commits nothing.
                if index > 0 {
                    return Ok(());
                }
                let Some(documents) = documents.as_mut() else {
                    return Ok(());
                };
                match Self::dry_run(self.collection, &committed, &pending, documents) {
                    Ok(dry_run) => {
                        report.dry_run = Some(dry_run);
                        Ok(())
                    }
                    Err(error) => {
                        let stand_in = fylo_storage_native::NativeStorageError::codec_rejected(
                            error.to_string(),
                        );
                        failure = Some(error);
                        Err(stand_in)
                    }
                }
            };
            let outcome = self
                .registry
                .commit(self.collection, &version.record, &mut verify);
            if let Err(error) = outcome {
                return Err(failure.unwrap_or_else(|| EngineError::storage(error)));
            }
            report.pushed.push(PushedVersion {
                ordinal: version.ordinal(),
                label: version.label().to_owned(),
                sha256: version.record.sha256.clone(),
                portable: version.record.portable.unwrap_or(false),
            });
            let non_portable = version.non_portable_patterns();
            if !non_portable.is_empty() {
                report.warnings.push(format!(
                    "schema {} of {} is not portable: {} cannot be compiled to a DFA, so \
                     builds without a regex compiler cannot validate it",
                    version.label(),
                    self.collection,
                    non_portable.join(", ")
                ));
            }
            report.head = Some(version.label().to_owned());
        }
        Ok(report)
    }

    /// The folder's versions the registry lacks, built as the records they
    /// will be committed as, and how many it already holds.
    fn pending(
        &self,
        committed: &[Arc<LoadedVersion>],
        actor: &str,
    ) -> Result<(Vec<Arc<LoadedVersion>>, usize), EngineError> {
        let manifest = SchemaManifest::load(&self.paths)
            .map_err(EngineError::schema)?
            .ok_or_else(|| {
                EngineError::schema(format!(
                    "no schema manifest for {} at {}",
                    self.collection,
                    self.paths.manifest().display()
                ))
            })?;
        let current = manifest
            .versions
            .iter()
            .position(|entry| entry.matches(&manifest.current))
            .ok_or_else(|| {
                EngineError::schema(format!(
                    "Manifest for '{}': 'current' ({}) is not present in 'versions'",
                    self.collection, manifest.current
                ))
            })?;
        let mut pending: Vec<Arc<LoadedVersion>> = Vec::new();
        let mut unchanged = 0;
        let committed_at = fylo_storage_native::utc_timestamp().map_err(EngineError::storage)?;
        for entry in &manifest.versions[..=current] {
            let label = &entry.version;
            let chex = self.folder_schema(label)?;
            SchemaRules::check(self.collection, label, &chex)?;
            let sha256 = RegistryVersion::digest(&chex);
            if let Some(existing) = committed.iter().find(|version| version.label() == label) {
                self.require_same(existing, &sha256, !pending.is_empty())?;
                unchanged += 1;
                continue;
            }
            let previous = pending
                .last()
                .or_else(|| committed.last())
                .map(|version| version.label().to_owned());
            let ordinal = u32::try_from(committed.len() + pending.len() + 1)
                .map_err(|_| EngineError::schema("schema registry is full".into()))?;
            let migration = previous
                .as_deref()
                .map(|previous| self.folder_migration(previous, label))
                .transpose()?;
            let record = RegistryVersion {
                format: RegistryVersion::FORMAT.into(),
                ordinal,
                label: label.clone(),
                chex,
                sha256,
                migration,
                compiled: None,
                portable: None,
                committed: Committed {
                    at: committed_at.clone(),
                    actor: actor.to_owned(),
                },
            };
            let loaded = LoadedVersion::compiled_record(self.collection, record)
                .map_err(EngineError::schema)?;
            pending.push(Arc::new(loaded));
        }
        self.require_current_is_head(committed, &pending, &manifest.current)?;
        Ok((pending, unchanged))
    }

    fn require_same(
        &self,
        existing: &LoadedVersion,
        sha256: &str,
        after_pending: bool,
    ) -> Result<(), EngineError> {
        if existing.record.sha256 != sha256 {
            return Err(EngineError::new(
                EngineErrorCode::SchemaConflict,
                format!(
                    "schema {} of {} is already committed as version {} with sha256 {}, and the folder's is {sha256}; \
                     committed versions are immutable, so give the change a new label",
                    existing.label(),
                    self.collection,
                    existing.ordinal(),
                    existing.record.sha256
                ),
            ));
        }
        if after_pending {
            return Err(EngineError::new(
                EngineErrorCode::SchemaConflict,
                format!(
                    "the manifest of {} lists a new version before the committed version {}; \
                     the registry appends versions after its head",
                    self.collection,
                    existing.label()
                ),
            ));
        }
        Ok(())
    }

    /// A manifest whose `current` is an older committed version would move
    /// the head back, which the registry does only as a new version.
    fn require_current_is_head(
        &self,
        committed: &[Arc<LoadedVersion>],
        pending: &[Arc<LoadedVersion>],
        current: &str,
    ) -> Result<(), EngineError> {
        let head = pending.last().or_else(|| committed.last());
        match head {
            Some(head) if head.label() != current => Err(EngineError::new(
                EngineErrorCode::SchemaConflict,
                format!(
                    "the manifest of {} names {current} as current, but the registry head is {}; \
                     a rollback is a new version",
                    self.collection,
                    head.label()
                ),
            )),
            _ => Ok(()),
        }
    }

    fn folder_schema(&self, label: &str) -> Result<Value, EngineError> {
        let path = self.paths.version(label);
        let bytes = SchemaFiles::read(&path)
            .map_err(|error| EngineError::schema(format!("{error}: {}", path.display())))?;
        serde_json::from_slice(&bytes).map_err(|error| {
            EngineError::schema(format!(
                "schema {} is not valid JSON: {error}",
                path.display()
            ))
        })
    }

    fn folder_migration(&self, from: &str, to: &str) -> Result<Value, EngineError> {
        let legacy = self.paths.legacy_upgrader(from, to);
        if legacy.is_file() {
            return Err(SchemaRules::javascript(
                self.collection,
                to,
                &format!("a JavaScript upgrader at {}", legacy.display()),
            ));
        }
        let path = self.paths.migration(from, to);
        let bytes = SchemaFiles::read(&path).map_err(|_| {
            EngineError::schema(format!(
                "Missing migration {from}->{to} for collection '{}' at {}",
                self.collection,
                path.display()
            ))
        })?;
        let migration: Value = serde_json::from_slice(&bytes).map_err(|error| {
            EngineError::schema(format!(
                "migration {} is not valid JSON: {error}",
                path.display()
            ))
        })?;
        SchemaRules::check_migration(self.collection, to, &migration)?;
        Ok(migration)
    }

    /// Migrate each document through every pending version and validate it
    /// at the last, the version the push makes the head, refusing the push
    /// with the first failures.
    fn dry_run(
        collection: &str,
        committed: &[Arc<LoadedVersion>],
        pending: &[Arc<LoadedVersion>],
        documents: DryRunDocuments<'_>,
    ) -> Result<DryRunReport, EngineError> {
        let Some(target) = pending.last() else {
            return Ok(DryRunReport {
                checked: 0,
                total: 0,
            });
        };
        let (total, sample) = documents()?;
        let chain: Vec<_> = committed.iter().chain(pending).cloned().collect();
        let mut failures = Vec::new();
        for (identifier, fields) in &sample {
            let outcome = SchemaRegistry::materialize_through(collection, &chain, fields).and_then(
                |mut migrated| {
                    migrated.remove(SchemaRegistry::VERSION_FIELD);
                    target.validate(collection, &migrated)
                },
            );
            if let Err(error) = outcome {
                failures.push(format!("{identifier}: {error}"));
                if failures.len() == Self::REPORTED_FAILURES {
                    break;
                }
            }
        }
        if !failures.is_empty() {
            return Err(EngineError::schema(format!(
                "push of {collection} refused: migrated documents fail {}: {}",
                target.label(),
                failures.join("; ")
            )));
        }
        Ok(DryRunReport {
            checked: sample.len(),
            total,
        })
    }

    /// Every id below the dry-run limit, else an evenly spaced sample.
    pub(crate) fn sample<T>(documents: Vec<T>) -> Vec<T> {
        let total = documents.len();
        if total <= Self::DRY_RUN_DOCUMENTS {
            return documents;
        }
        let stride = total.div_ceil(Self::DRY_RUN_DOCUMENTS);
        documents.into_iter().step_by(stride).collect()
    }
}
