use std::ops::Range;

use fylo_format::Document;
use serde_json::{Map, Value};

use super::schema_manifest::SchemaManifest;
use super::schema_migration::SchemaMigration;
use super::schema_paths::SchemaPaths;

pub(super) struct SchemaUpgrade<'a> {
    paths: &'a SchemaPaths<'a>,
    manifest: &'a SchemaManifest,
}

impl<'a> SchemaUpgrade<'a> {
    pub(super) const VERSION_FIELD: &'static str = "_v";

    pub(super) fn new(paths: &'a SchemaPaths<'a>, manifest: &'a SchemaManifest) -> Self {
        Self { paths, manifest }
    }

    pub(super) fn materialize(
        &self,
        document: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        self.require_versions()?;
        let from = document
            .get(Self::VERSION_FIELD)
            .and_then(Value::as_str)
            .unwrap_or(&self.manifest.versions[0].version);
        let start = self.start_index(from)?;
        let end = self.end_index()?;
        self.check_order(from, start, end)?;
        self.materialize_range(document, start..end)
    }

    fn require_versions(&self) -> Result<(), String> {
        if self.manifest.versions.is_empty() {
            return Err(format!(
                "Invalid manifest for '{}': 'versions' must be a non-empty array",
                self.paths.collection
            ));
        }
        Ok(())
    }

    fn start_index(&self, from: &str) -> Result<usize, String> {
        self.manifest
            .versions
            .iter()
            .position(|entry| entry.matches(from))
            .ok_or_else(|| {
                format!(
                    "Doc in '{}' is at unknown version '{from}' (not in manifest.versions)",
                    self.paths.collection
                )
            })
    }

    fn end_index(&self) -> Result<usize, String> {
        self.manifest
            .versions
            .iter()
            .position(|entry| entry.matches(&self.manifest.current))
            .ok_or_else(|| {
                format!(
                    "Manifest for '{}': 'current' ({}) is not present in 'versions'",
                    self.paths.collection, self.manifest.current
                )
            })
    }

    fn check_order(&self, from: &str, start: usize, end: usize) -> Result<(), String> {
        if start > end {
            return Err(format!(
                "Doc in '{}' is at {from}, ahead of target {}: schema rolled back?",
                self.paths.collection, self.manifest.current
            ));
        }
        Ok(())
    }

    fn materialize_range(
        &self,
        document: &Map<String, Value>,
        range: Range<usize>,
    ) -> Result<Map<String, Value>, String> {
        if range.is_empty() {
            return Ok(Document::copy_fields(document));
        }
        self.upgraded(document, range)
    }

    fn upgraded(
        &self,
        document: &Map<String, Value>,
        range: Range<usize>,
    ) -> Result<Map<String, Value>, String> {
        let mut next = Document::copy_fields(document);
        next.remove(Self::VERSION_FIELD);
        next = self.run_chain(next, range)?;
        next.insert(
            Self::VERSION_FIELD.into(),
            Value::String(self.manifest.current.clone()),
        );
        Ok(next)
    }

    fn run_chain(
        &self,
        mut document: Map<String, Value>,
        range: Range<usize>,
    ) -> Result<Map<String, Value>, String> {
        for index in range {
            let from = &self.manifest.versions[index].version;
            let to = &self.manifest.versions[index + 1].version;
            SchemaMigration::load(self.paths, from, to)?.apply(&mut document)?;
        }
        Ok(document)
    }
}
