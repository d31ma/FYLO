use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};

use super::registry::{LoadedVersion, SchemaRegistry};
use super::schema_manifest::SchemaManifest;
use super::schema_migration::SchemaMigration;
use super::schema_paths::SchemaPaths;
use super::schema_tools::SchemaTools;

/// A collection's schema versions, oldest first, when it has a migration to
/// run: what a read needs to return an old document in the head shape (ADR
/// 0017 section 5, lazy migration).
///
/// The versions come from the collection's schema registry when it has one,
/// else from its `fylo.schema` folder. A collection with one version, or
/// none, has no chain, and its documents are read as they are stored.
pub(crate) struct VersionChain {
    labels: Vec<String>,
    source: ChainSource,
}

enum ChainSource {
    Registry(Vec<Arc<LoadedVersion>>),
    Folder(PathBuf),
}

impl VersionChain {
    const VERSION_FIELD: &'static str = "_v";

    /// `collection`'s chain, or `None` when it has fewer than two versions.
    pub(crate) fn load(
        registry: &SchemaRegistry,
        folder: Option<&Path>,
        collection: &str,
    ) -> Result<Option<Self>, String> {
        let versions = registry.versions(collection)?;
        if !versions.is_empty() {
            if versions.len() < 2 {
                return Ok(None);
            }
            let labels = versions
                .iter()
                .map(|version| version.label().to_owned())
                .collect();
            return Ok(Some(Self {
                labels,
                source: ChainSource::Registry(versions),
            }));
        }
        let Some(folder) = folder else {
            return Ok(None);
        };
        let paths = SchemaPaths::new(folder, collection);
        let Some(manifest) = SchemaManifest::load(&paths)? else {
            return Ok(None);
        };
        let Some(end) = manifest
            .versions
            .iter()
            .position(|entry| entry.matches(&manifest.current))
        else {
            return Err(format!(
                "Manifest for '{collection}': 'current' ({}) is not present in 'versions'",
                manifest.current
            ));
        };
        if end == 0 {
            return Ok(None);
        }
        Ok(Some(Self {
            labels: manifest.versions[..=end]
                .iter()
                .map(|entry| entry.version.clone())
                .collect(),
            source: ChainSource::Folder(folder.to_owned()),
        }))
    }

    pub(crate) fn head(&self) -> &str {
        self.labels.last().map_or("", String::as_str)
    }

    pub(crate) fn oldest(&self) -> &str {
        self.labels.first().map_or("", String::as_str)
    }

    pub(crate) fn labels(&self) -> &[String] {
        &self.labels
    }

    /// Whether `document` is stored at the head, so a read returns it as it
    /// is. This is the whole cost of a read once a collection has converged.
    pub(crate) fn is_at_head(&self, document: &Map<String, Value>) -> bool {
        document.get(Self::VERSION_FIELD).and_then(Value::as_str) == Some(self.head())
    }

    /// The version `document` was written at: its `_v`, or the oldest when it
    /// has none.
    pub(crate) fn written_at<'a>(&'a self, document: &'a Map<String, Value>) -> &'a str {
        document
            .get(Self::VERSION_FIELD)
            .and_then(Value::as_str)
            .unwrap_or_else(|| self.oldest())
    }

    /// `document`, decrypted, migrated to the head by the JSON steps of every
    /// version after the one it was written at.
    pub(crate) fn migrate(
        &self,
        collection: &str,
        document: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        match &self.source {
            ChainSource::Registry(versions) => {
                SchemaRegistry::materialize_through(collection, versions, document)
            }
            ChainSource::Folder(folder) => {
                SchemaTools::new(folder).materialize(collection, document)
            }
        }
    }

    /// The top-level fields that migrating a document stored at `label`, or
    /// at any later version, may write or remove, `_v` included. An index
    /// built from stored documents is right for every other field. A label
    /// the chain does not know counts as the oldest.
    pub(crate) fn fields_migrated_from(
        &self,
        collection: &str,
        label: &str,
    ) -> Result<BTreeSet<String>, String> {
        let start = self
            .labels
            .iter()
            .position(|known| known == label)
            .unwrap_or(0);
        let mut fields = BTreeSet::from([Self::VERSION_FIELD.to_owned()]);
        for index in start + 1..self.labels.len() {
            match &self.source {
                ChainSource::Registry(versions) => {
                    fields.extend(versions[index].migrated_fields());
                }
                ChainSource::Folder(folder) => {
                    let paths = SchemaPaths::new(folder, collection);
                    let migration = SchemaMigration::load(
                        &paths,
                        &self.labels[index - 1],
                        &self.labels[index],
                    )?;
                    fields.extend(migration.written_fields());
                }
            }
        }
        Ok(fields)
    }
}
