#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_engine::{EngineErrorCode, ReadOnlyEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{
    CollectionKind, GenerationStatus, NativeStorageErrorCode, NativeWriteRoot, PutRawFileOptions,
};
use serde_json::json;

struct Bucket(PathBuf);

impl Bucket {
    const NAME: &'static str = "assets";
    const KEPT: &'static str = "4VRNF52J031";
    const STRIPPED: &'static str = "4VRNF52J032";
    const ADDED: &'static str = "4VRNF52J033";
    const CHILD: &'static str = "FYLO_STRIPPED_RAW_FILE_CHILD";
    const STUCK_TRANSACTION: &'static str = "rust-tx-77785-18e6071482559320-3";

    fn create() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fylo-stripped-raw-file-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let bucket = Self(root);
        let writer = bucket.writer();
        writer
            .create_collection(Self::NAME, CollectionKind::File, None)
            .unwrap();
        Self::put(&writer, Self::KEPT);
        Self::put(&writer, Self::STRIPPED);
        bucket
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(&self.0).unwrap()
    }

    fn put(writer: &NativeWriteRoot, identifier: &str) {
        writer
            .put_raw_file(
                Self::NAME,
                identifier,
                b"content",
                &PutRawFileOptions {
                    key: format!("/{identifier}.txt"),
                    extension: ".txt".into(),
                    metadata: BTreeMap::new(),
                },
            )
            .unwrap();
    }

    fn stored_file(&self, identifier: &str) -> PathBuf {
        let docs = self.0.join(".buckets").join(Self::NAME).join("docs");
        let name = format!("{identifier}.txt");
        fs::read_dir(&docs)
            .unwrap()
            .map(|shard| shard.unwrap().path().join(&name))
            .find(|path| path.exists())
            .unwrap()
    }

    fn strip_attributes(&self, identifier: &str) {
        let path = self.stored_file(identifier);
        let names: Vec<_> = fylo_xattr::ExtendedAttributes::list(&path)
            .unwrap()
            .filter(|name| name.to_string_lossy().starts_with("user."))
            .collect();
        for name in names {
            fylo_xattr::ExtendedAttributes::remove(&path, name).unwrap();
        }
    }

    fn journal(&self) -> PathBuf {
        self.0.join(".fylo-transactions/.buckets").join(Self::NAME)
    }

    fn generation_state(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.journal().join("state.json")).unwrap()).unwrap()
    }

    fn assert_readable(&self) {
        let engine = ReadOnlyEngine::open(&self.0).unwrap();
        let inspection = engine.inspect(Self::NAME).unwrap();
        assert_eq!(inspection.state, GenerationStatus::Stable);
        assert_eq!(self.generation_state()["state"], "stable");
        let query = StructuredQuery::from_value(
            &json!({"$ops": [{"key": {"$eq": format!("/{}.txt", Self::KEPT)}}]}),
            QueryLimits::default(),
        )
        .unwrap();
        let rows = engine.find_files(Self::NAME, &query).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, Self::KEPT);
    }

    fn assert_unreadable(&self) {
        let engine = ReadOnlyEngine::open(&self.0).unwrap();
        let error = engine.inspect(Self::NAME).unwrap_err();
        assert_eq!(error.code(), EngineErrorCode::ConcurrentWrite);
    }

    fn leave_rebuild_unfinished(&self) {
        let journal = self.journal();
        let stable = self.generation_state();
        let generation = stable["generation"].as_u64().unwrap();
        let transaction = journal.join(Self::STUCK_TRANSACTION);
        fs::create_dir_all(transaction.join("before")).unwrap();
        fs::create_dir_all(transaction.join("captures")).unwrap();
        let index = self.0.join(".buckets").join(Self::NAME).join("index");
        for (position, name) in ["keys.snapshot", "keys.wal"].iter().enumerate() {
            Self::capture_index_file(&transaction, &index.join(name), name, position);
        }
        let manifest = json!({
            "format": "fylo.collection-transaction.v1",
            "id": Self::STUCK_TRANSACTION,
            "collection": Self::NAME,
            "operation": "rebuild-index",
            "phase": "active",
            "generationBefore": generation,
            "eventOffset": 0,
            "captures": []
        });
        fs::write(transaction.join("transaction.json"), manifest.to_string()).unwrap();
        let writing = json!({
            "format": "fylo.collection-generation.v1",
            "generation": generation + 1,
            "state": "writing",
            "transactionId": Self::STUCK_TRANSACTION
        });
        fs::write(journal.join("state.json"), writing.to_string()).unwrap();
    }

    fn capture_index_file(transaction: &Path, file: &Path, name: &str, position: usize) {
        let backup = format!("before/{position:06}.bin");
        let present = file.exists();
        let capture = json!({
            "path": format!("index/{name}"),
            "present": present,
            "backup": present.then_some(backup.clone()),
            "mode": present.then_some(0o644)
        });
        fs::write(
            transaction.join(format!("captures/{position:06}.json")),
            capture.to_string(),
        )
        .unwrap();
        fs::copy(file, transaction.join(&backup)).ok();
    }

    fn run_child_with_failpoint(&self, failpoint: &str) {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "failing_write_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(Self::CHILD, &self.0)
            .env("FYLO_RUST_FAILPOINT", failpoint)
            .env("FYLO_RUST_FAILPOINT_ACTION", "enospc")
            .status()
            .unwrap();
        assert!(status.success());
    }
}

impl Drop for Bucket {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_rebuild_that_meets_a_stripped_file_names_it_and_leaves_the_bucket_readable() {
    let bucket = Bucket::create();
    bucket.strip_attributes(Bucket::STRIPPED);
    let error = bucket
        .writer()
        .rebuild_collection(Bucket::NAME)
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::CorruptMetadata);
    let relative = bucket
        .stored_file(Bucket::STRIPPED)
        .strip_prefix(&bucket.0)
        .unwrap()
        .display()
        .to_string();
    let canonical_root = fs::canonicalize(&bucket.0).unwrap();
    assert!(error.message().contains(&relative), "{}", error.message());
    assert!(error.message().contains("putData"), "{}", error.message());
    assert!(
        !error.message().contains(&*bucket.0.to_string_lossy()),
        "{}",
        error.message()
    );
    assert!(
        !error.message().contains(&*canonical_root.to_string_lossy()),
        "{}",
        error.message()
    );
    bucket.assert_readable();
}

#[test]
fn a_bucket_left_writing_by_an_unfinished_rebuild_recovers_on_the_next_writer_open() {
    let bucket = Bucket::create();
    bucket.strip_attributes(Bucket::STRIPPED);
    bucket.leave_rebuild_unfinished();
    bucket.assert_unreadable();
    bucket.writer().recover_collection(Bucket::NAME).unwrap();
    bucket.assert_readable();
}

#[test]
fn a_rolled_back_write_beside_a_stripped_file_leaves_the_bucket_readable() {
    let bucket = Bucket::create();
    bucket.strip_attributes(Bucket::STRIPPED);
    bucket.run_child_with_failpoint("before-commit-marker");
    bucket.assert_readable();
    assert!(
        fs::read_dir(bucket.0.join(".buckets").join(Bucket::NAME).join("docs"))
            .unwrap()
            .all(|shard| !shard
                .unwrap()
                .path()
                .join(format!("{}.txt", Bucket::ADDED))
                .exists())
    );
}

#[test]
fn failing_write_child() {
    let Ok(root) = std::env::var(Bucket::CHILD) else {
        return;
    };
    let writer = NativeWriteRoot::open(&root).unwrap();
    let error = writer
        .put_raw_file(
            Bucket::NAME,
            Bucket::ADDED,
            b"content",
            &PutRawFileOptions {
                key: format!("/{}.txt", Bucket::ADDED),
                extension: ".txt".into(),
                metadata: BTreeMap::new(),
            },
        )
        .unwrap_err();
    assert_ne!(error.code(), NativeStorageErrorCode::CorruptMetadata);
}
