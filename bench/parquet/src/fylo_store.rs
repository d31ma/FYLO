use std::path::{Path, PathBuf};

use fylo_engine::{ReadOnlyEngine, WriteEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{
    CollectionKind, CollectionLayout, FilesystemProfile, NativeStorageErrorCode, NativeWriteRoot,
    RootConfig, RootLease, RootOpenOptions, WriteAccess,
};
use serde_json::{Value, json};

use crate::data::Doc;
use crate::sizes::Footprint;
use crate::store::{Filter, Outcome, Store};

/// FYLO's per-document JSON file layout through its own engine, called the way the `fylo` crate and the
/// machine protocol call it: one engine open per call, under the root's lease
/// for a directory root, and every write durable before it returns.
pub struct FyloStore {
    root: PathBuf,
    lease: Option<RootLease>,
    collection: String,
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn wide_limits() -> QueryLimits {
    let mut limits = QueryLimits::default();
    limits.max_snapshot_bytes = 8 * 1024 * 1024 * 1024;
    limits.max_matches = 100_000_000;
    limits.max_output_bytes = 16 * 1024 * 1024 * 1024;
    limits
}

impl FyloStore {
    pub fn path_in(directory: &Path) -> PathBuf {
        directory.join("root")
    }

    pub fn create(directory: &Path) -> Outcome<Self> {
        let root = Self::path_in(directory);
        std::fs::create_dir_all(&root).map_err(text)?;
        let store = Self::lease_path(root)?;
        store
            .writer()?
            .create_collection_with_layout(
                &store.collection,
                CollectionKind::Document,
                None,
                Some(CollectionLayout::Files),
            )
            .map_err(text)?;
        Ok(store)
    }

    pub fn open(directory: &Path) -> Outcome<Self> {
        Self::open_path(Self::path_in(directory))
    }

    pub fn open_path(root: PathBuf) -> Outcome<Self> {
        let store = Self::lease_path(root)?;
        store.recover()?;
        Ok(store)
    }

    fn lease_path(root: PathBuf) -> Outcome<Self> {
        let mut store = Self {
            root,
            lease: None,
            collection: "items".to_owned(),
        };
        store.take_lease()?;
        Ok(store)
    }

    fn recover(&self) -> Outcome<()> {
        if std::env::var_os("BENCH_FYLO_SKIP_RECOVERY").is_some() {
            return Ok(());
        }
        self.writer()?
            .recover_collection(&self.collection)
            .map(|_| ())
            .map_err(text)
    }

    fn take_lease(&mut self) -> Outcome<()> {
        let lease =
            RootLease::acquire_with_profile(&self.root, FilesystemProfile::Native).map_err(text)?;
        self.root = lease.root().to_path_buf();
        self.lease = Some(lease);
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn options(&self) -> RootOpenOptions {
        let options = RootOpenOptions::new(&self.root).with_profile(FilesystemProfile::Native);
        match &self.lease {
            Some(lease) => options.with_lease(lease.clone()),
            None => options,
        }
    }

    pub fn writer(&self) -> Outcome<NativeWriteRoot> {
        Ok(NativeWriteRoot::open_profiled(self.options())
            .map_err(text)?
            .with_config(RootConfig::default()))
    }

    fn write_engine(&self) -> Outcome<WriteEngine> {
        Ok(WriteEngine::open_profiled(self.options())
            .map_err(text)?
            .with_config(RootConfig::default()))
    }

    fn reader(&self) -> Outcome<ReadOnlyEngine> {
        ReadOnlyEngine::open_profiled(self.options()).map_err(text)
    }

    fn query(&self, query: &Value) -> Outcome<Vec<(String, Doc)>> {
        let parsed = StructuredQuery::from_value(query, wide_limits()).map_err(text)?;
        let rows = self
            .reader()?
            .find(&self.collection, &parsed)
            .map_err(text)?;
        Ok(rows
            .into_iter()
            .map(|record| (record.metadata.id, record.document.into_fields()))
            .collect())
    }

    pub fn rebuild_indexes(&self) -> Outcome<()> {
        self.writer()?
            .rebuild_collection(&self.collection)
            .map_err(text)
    }

    pub fn close(self) -> Outcome<()> {
        match self.lease {
            Some(lease) => lease.release_checked().map_err(text),
            None => Ok(()),
        }
    }
}

impl Store for FyloStore {
    fn put(&mut self, doc: Doc) -> Outcome<String> {
        let writer = self.writer()?;
        let identifier = writer.allocate_identifier().map_err(text)?;
        WriteEngine::from_writer(writer)
            .put_document(&self.collection, &identifier, doc, WriteAccess::default())
            .map_err(text)?;
        Ok(identifier)
    }

    fn put_batch(&mut self, docs: Vec<Doc>) -> Outcome<Vec<String>> {
        let writer = self.writer()?;
        let engine = self.write_engine()?;
        let mut identifiers = Vec::with_capacity(docs.len());
        for doc in docs {
            let identifier = writer.allocate_identifier().map_err(text)?;
            engine
                .put_document(&self.collection, &identifier, doc, WriteAccess::default())
                .map_err(text)?;
            identifiers.push(identifier);
        }
        Ok(identifiers)
    }

    fn patch(&mut self, id: &str, changes: &Doc) -> Outcome<()> {
        self.write_engine()?
            .patch_document_fields(&self.collection, id, changes, None)
            .map_err(text)
    }

    fn delete(&mut self, id: &str) -> Outcome<()> {
        self.writer()?
            .delete_document(&self.collection, id, None)
            .map_err(text)
    }

    fn get(&mut self, id: &str) -> Outcome<Option<Doc>> {
        let found = self.reader()?.get(&self.collection, id);
        match found {
            Ok(record) => Ok(Some(record.document.into_fields())),
            Err(error) if error.storage_code() == Some(NativeStorageErrorCode::NotFound) => {
                Ok(None)
            }
            Err(error) => Err(text(error)),
        }
    }

    fn latest(&mut self, id: &str) -> Outcome<Option<Doc>> {
        let found = self.reader()?.get(&self.collection, id);
        match found {
            Ok(record) => Ok(Some(record.document.into_fields())),
            Err(_) => Ok(None),
        }
    }

    fn find_eq(&mut self, field: &str, value: i64) -> Outcome<Vec<(String, Doc)>> {
        self.query(&json!({ "$ops": [{ field: { "$eq": value } }] }))
    }

    fn find_range(&mut self, field: &str, low: f64, high: f64) -> Outcome<Vec<(String, Doc)>> {
        self.query(&json!({ "$ops": [{ field: { "$gte": low, "$lt": high } }] }))
    }

    fn scan_project(&mut self, filter: Option<&Filter>, fields: &[&str]) -> Outcome<Vec<Doc>> {
        let query = match filter {
            Some(filter) => json!({ "$ops": [{ filter.field: { "$gte": filter.minimum } }] }),
            None => json!({}),
        };
        let rows = self.query(&query)?;
        Ok(rows
            .iter()
            .map(|(_, doc)| crate::data::project(doc, fields))
            .collect())
    }

    fn count(&mut self) -> Outcome<usize> {
        let inspected = self.reader()?.inspect(&self.collection).map_err(text)?;
        Ok(inspected.document_count as usize)
    }

    fn compact(&mut self) -> Outcome<Value> {
        Ok(json!({ "skipped": "the file layout has no compaction" }))
    }

    fn reopen(&mut self) -> Outcome<()> {
        if let Some(lease) = self.lease.take() {
            lease.release_checked().map_err(text)?;
        }
        self.take_lease()?;
        self.recover()
    }

    fn file_count(&self) -> usize {
        Footprint::measure(&self.root).files as usize
    }
}
