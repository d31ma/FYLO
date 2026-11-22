use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use fylo_format::LogRecordKind;

use super::namespace_log::NamespaceLog;
use super::namespace_manifest::NamespaceManifest;
use crate::NativeStorageError;

/// A reader's view of a namespace log: for each collection it names, the
/// highest generation of its parts (ADR 0013, "Readers after a power cut").
///
/// A reader reads the log at its first read of the namespace, when it sees
/// a collection's generation go backwards, and before it would hold a
/// collection back, so a settled log releases it at once.
#[derive(Debug, Default)]
pub(crate) struct NamespaceView {
    read: bool,
    parts: BTreeMap<String, u64>,
    seen: BTreeMap<String, u64>,
}

impl NamespaceView {
    /// Whether `collection`, stable at `generation`, lags a committed part
    /// the namespace log holds for it: a reader must treat it as `writing`.
    pub(crate) fn lags(
        root: &Path,
        collection: &str,
        generation: u64,
    ) -> Result<bool, NativeStorageError> {
        let views = Self::registry();
        let mut views = views.lock().unwrap_or_else(PoisonError::into_inner);
        let view = views.entry(root.to_owned()).or_default();
        let backwards = view.observe(collection, generation);
        if !view.read || backwards || view.would_hold(collection, generation) {
            view.refresh(root)?;
        }
        Ok(view.would_hold(collection, generation))
    }

    fn registry() -> &'static Mutex<BTreeMap<PathBuf, Self>> {
        static VIEWS: OnceLock<Mutex<BTreeMap<PathBuf, NamespaceView>>> = OnceLock::new();
        VIEWS.get_or_init(Mutex::default)
    }

    /// Remember the highest generation seen; true when this one is lower.
    fn observe(&mut self, collection: &str, generation: u64) -> bool {
        let seen = self.seen.entry(collection.to_owned()).or_insert(generation);
        let backwards = generation < *seen;
        *seen = (*seen).max(generation);
        backwards
    }

    fn would_hold(&self, collection: &str, generation: u64) -> bool {
        self.parts
            .get(collection)
            .is_some_and(|highest| *highest > generation)
    }

    /// Read the namespace log afresh: an open and a read, never cached
    /// attributes.
    fn refresh(&mut self, root: &Path) -> Result<(), NativeStorageError> {
        self.read = true;
        self.parts.clear();
        let Some(scan) = NamespaceLog::scan(root)? else {
            return Ok(());
        };
        for (_, record) in &scan.records {
            if record.kind != LogRecordKind::Commit {
                continue;
            }
            let Ok(manifest) = serde_json::from_slice::<NamespaceManifest>(&record.manifest) else {
                continue;
            };
            for part in manifest.parts {
                let highest = self.parts.entry(part.collection).or_insert(0);
                *highest = (*highest).max(part.generation);
            }
        }
        Ok(())
    }
}
