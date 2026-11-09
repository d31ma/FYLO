#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_engine::{EngineErrorCode, ReadOnlyEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{CollectionKind, NativeWriteRoot, PutRawFileOptions};
use serde_json::{Value, json};

struct StrandedRawFile(PathBuf);

impl StrandedRawFile {
    const NAME: &'static str = "assets";
    const COMMITTED: [&'static str; 2] = ["4VRNF52J041", "4VRNF52J042"];
    const STRANDED: &'static str = "4VRNF52J043";
    const CHILD: &'static str = "FYLO_STRANDED_RAW_FILE_CHILD";

    fn create() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fylo-stranded-raw-file-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let bucket = Self(root);
        let writer = NativeWriteRoot::open(&bucket.0).unwrap();
        writer
            .create_collection(Self::NAME, CollectionKind::File, None)
            .unwrap();
        for identifier in Self::COMMITTED {
            Self::put(&writer, identifier);
        }
        bucket
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

    fn stored(&self, identifier: &str) -> bool {
        let docs = self.0.join(".buckets").join(Self::NAME).join("docs");
        let name = format!("{identifier}.txt");
        fs::read_dir(&docs)
            .unwrap()
            .any(|shard| shard.unwrap().path().join(&name).exists())
    }

    fn crash_put_at(&self, failpoint: &str) {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "crashing_put_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(Self::CHILD, &self.0)
            .env("FYLO_RUST_FAILPOINT", failpoint)
            .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
            .status()
            .unwrap();
        assert!(!status.success(), "the put did not stop at {failpoint}");
    }

    fn verify(&self) -> Value {
        NativeWriteRoot::open(&self.0)
            .unwrap()
            .verify_file_collection(Self::NAME)
            .unwrap()
    }

    fn found_files(&self) -> usize {
        let engine = ReadOnlyEngine::open(&self.0).unwrap();
        let query = StructuredQuery::from_value(&json!({}), QueryLimits::default()).unwrap();
        engine.find_files(Self::NAME, &query).unwrap().len()
    }

    fn read_error(&self) -> EngineErrorCode {
        ReadOnlyEngine::open(&self.0)
            .unwrap()
            .inspect(Self::NAME)
            .unwrap_err()
            .code()
    }
}

impl Drop for StrandedRawFile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn verify_after_a_crash_mid_put_counts_only_committed_files_like_find() {
    let bucket = StrandedRawFile::create();
    bucket.crash_put_at("before-commit-marker");
    assert!(bucket.stored(StrandedRawFile::STRANDED));
    assert_eq!(bucket.read_error(), EngineErrorCode::ConcurrentWrite);

    let report = bucket.verify();

    assert_eq!(report["filesScanned"], 2, "{report}");
    assert_eq!(report["verified"], 2, "{report}");
    assert_eq!(report["corrupt"], json!([]), "{report}");
    assert!(!bucket.stored(StrandedRawFile::STRANDED));
    assert_eq!(bucket.found_files(), 2);
}

#[test]
fn crashing_put_child() {
    let Ok(root) = std::env::var(StrandedRawFile::CHILD) else {
        return;
    };
    let writer = NativeWriteRoot::open(&root).unwrap();
    StrandedRawFile::put(&writer, StrandedRawFile::STRANDED);
}
