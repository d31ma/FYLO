use crate::root_damage::RootDamage;
use crate::root_verification_report::RootVerificationReport;
use crate::{CollectionKind, NativeCollection, NativeRoot, NativeStorageError, NativeWriteRoot};
use std::collections::BTreeSet;
use std::path::Path;

/// Walk every collection of a folder root and read every live document and
/// file, so that every file a storage crash damaged is found at once
/// (L1 spec 20.2, `fylo verify --root <root>`).
///
/// Each collection first finishes a transaction a crash interrupted, as the
/// next writer would: an unfinished one rolls back, a committed one rolls
/// forward. Only then are its files read, so a crash in the middle of a
/// write is never reported as damage.
///
/// A document that cannot be read or is not JSON, a file whose bytes do not
/// match its checksum, and an index that names a document that is gone are
/// each listed with the file and its code. Nothing is repaired: an
/// index is rebuilt with `rebuildCollection`, and a document is restored
/// from a backup.
pub struct RootVerification<'a> {
    writer: &'a NativeWriteRoot,
    root: &'a NativeRoot,
    report: RootVerificationReport,
}

impl<'a> RootVerification<'a> {
    const COLLECTION_FOLDERS: [&'static str; 2] = [".collections", ".buckets"];
    const NOT_JSON: &'static str = "ENATIVE_DOCUMENT";

    #[must_use]
    pub fn run(writer: &'a NativeWriteRoot, root: &'a NativeRoot) -> RootVerificationReport {
        let mut verification = Self {
            writer,
            root,
            report: RootVerificationReport {
                root: root.path().display().to_string(),
                ..RootVerificationReport::default()
            },
        };
        for name in Self::collection_names(root.path()) {
            verification.check_collection(&name);
        }
        verification.report.ok = verification.report.damaged.is_empty();
        verification.report
    }

    fn collection_names(root: &Path) -> BTreeSet<String> {
        Self::COLLECTION_FOLDERS
            .iter()
            .flat_map(|folder| Self::folder_names(&root.join(folder)))
            .collect()
    }

    // `FileType::is_dir` takes `self` on the browser target but `&self` on
    // wasi/native, so the closure is only redundant on the browser target.
    #[allow(clippy::redundant_closure_for_method_calls)]
    fn folder_names(folder: &Path) -> Vec<String> {
        let Ok(entries) = byckr_vfs::read_dir(folder) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.'))
            .collect()
    }

    fn check_collection(&mut self, name: &str) {
        self.report.collections_checked += 1;
        if let Err(error) = self.writer.recover_collection(name) {
            self.damage(name, &self.root.path().join(name), &error);
        }
        match self.root.collection(name) {
            Ok(collection) => self.check_kind(&collection),
            Err(error) => self.damage(name, &self.root.path().join(name), &error),
        }
    }

    fn check_kind(&mut self, collection: &NativeCollection) {
        if collection.kind == CollectionKind::File {
            self.check_raw_files(collection);
            return;
        }
        self.check_documents(collection);
        self.check_file_layout_index(collection);
    }

    /// Segment collections keep their index inside their segments, which
    /// reading every document already checks.
    fn check_file_layout_index(&mut self, collection: &NativeCollection) {
        if collection.uses_segments() {
            return;
        }
        self.check_index(collection);
    }

    fn check_documents(&mut self, collection: &NativeCollection) {
        match collection.document_ids() {
            Ok(identifiers) => identifiers
                .iter()
                .for_each(|identifier| self.check_document(collection, identifier)),
            Err(error) => self.damage(collection.name(), collection.path(), &error),
        }
    }

    fn check_document(&mut self, collection: &NativeCollection, identifier: &str) {
        self.report.files_checked += 1;
        match collection.read_document(identifier) {
            Ok(stored) => self.check_json(collection.name(), &stored.path, &stored.bytes),
            Err(error) => self.damage(
                collection.name(),
                &collection.path().join(identifier),
                &error,
            ),
        }
    }

    fn check_json(&mut self, collection: &str, path: &Path, bytes: &[u8]) {
        let Err(error) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            return;
        };
        let message = format!("damaged FYLO document file {}: {error}", path.display());
        self.report
            .damaged
            .push(RootDamage::new(collection, path, Self::NOT_JSON, message));
    }

    /// An index key that names a document that is gone (a rename that
    /// never reached the disk) fails the reference check with
    /// `ENATIVE_INDEX`.
    fn check_index(&mut self, collection: &NativeCollection) {
        if let Err(error) = collection.verify_index_references() {
            self.damage(collection.name(), collection.path(), &error);
        }
    }

    fn check_raw_files(&mut self, collection: &NativeCollection) {
        match collection.raw_file_ids() {
            Ok(identifiers) => identifiers
                .iter()
                .for_each(|identifier| self.check_raw_file(collection, identifier)),
            Err(error) => self.damage(collection.name(), collection.path(), &error),
        }
    }

    fn check_raw_file(&mut self, collection: &NativeCollection, identifier: &str) {
        self.report.files_checked += 1;
        if let Err(error) = collection.read_raw_file(identifier) {
            self.damage(
                collection.name(),
                &collection.path().join(identifier),
                &error,
            );
        }
    }

    fn damage(&mut self, collection: &str, file: &Path, error: &NativeStorageError) {
        self.report
            .damaged
            .push(RootDamage::from_error(collection, file, error));
    }
}
