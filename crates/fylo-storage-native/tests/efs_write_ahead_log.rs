//! EFS-profile document collections commit through the write-ahead log
//! (ADR 0013 Amendment 1): each record's sidecar is a step of the same
//! record, recovery replays both files, and a batch is one record.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_storage_native::{
    CollectionKind, DocumentWrite, FilesystemProfile, NativeRoot, NativeStorageErrorCode,
    NativeWriteRoot, PlainDocumentCodec, PutDocumentOptions, RootLease, RootOpenOptions,
};
use serde_json::{Map, Value, json};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const CHILD_ROOT: &str = "FYLO_TEST_EFS_CRASH_ROOT";

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    lease: Option<RootLease>,
}

impl Fixture {
    fn create() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = fylo_test_support::TestFolder::path()
            .canonicalize()
            .unwrap()
            .join(format!(
                "fylo-efs-wal-{}-{stamp}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        let root = parent.join("root");
        fs::create_dir_all(&root).unwrap();
        let lease = Some(efs_lease(&root));
        let fixture = Self {
            parent,
            root,
            lease,
        };
        fixture
            .writer()
            .create_collection("items", CollectionKind::Document, None)
            .unwrap();
        fixture
    }

    fn options(&self) -> RootOpenOptions {
        efs_options(&self.root, self.lease.clone().unwrap())
    }

    fn release_lease(&mut self) {
        self.lease = None;
    }

    fn take_lease(&mut self) {
        self.lease = Some(efs_lease(&self.root));
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(self.options()).unwrap()
    }

    fn reader(&self) -> NativeRoot {
        NativeRoot::open_profiled(self.options()).unwrap()
    }

    fn read(&self, identifier: &str) -> Value {
        let stored = self
            .reader()
            .collection("items")
            .unwrap()
            .read_document(identifier)
            .unwrap();
        serde_json::from_slice(&stored.bytes).unwrap()
    }

    fn metadata(&self, identifier: &str) -> BTreeMap<String, Value> {
        self.reader()
            .collection("items")
            .unwrap()
            .read_custom_metadata(identifier)
            .unwrap()
    }

    fn document_path(&self, identifier: &str) -> PathBuf {
        self.reader()
            .collection("items")
            .unwrap()
            .read_document(identifier)
            .unwrap()
            .path
    }

    fn transactions(&self) -> PathBuf {
        self.root.join(".fylo-transactions/.collections/items")
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.transactions().join("state.json")).unwrap()).unwrap()
    }

    fn log_records(&self) -> u64 {
        self.state()["log"]["records"].as_u64().unwrap()
    }

    fn mark_writing(&self) {
        let mut state = self.state();
        state["state"] = json!("writing");
        state["transactionId"] = json!("log-crash");
        fs::write(
            self.transactions().join("state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn efs_lease(root: &Path) -> RootLease {
    RootLease::acquire_with_profile(root, FilesystemProfile::EfsSidecarV1).unwrap()
}

fn efs_options(root: &Path, lease: RootLease) -> RootOpenOptions {
    RootOpenOptions::new(root)
        .with_profile(FilesystemProfile::EfsSidecarV1)
        .with_lease(lease)
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 40_000 + number)
}

fn sidecar(payload: &Path) -> PathBuf {
    let mut name = payload.as_os_str().to_os_string();
    name.push(".fylo-attrs");
    PathBuf::from(name)
}

fn put(writer: &NativeWriteRoot, number: usize) {
    writer
        .put_document(
            "items",
            &identifier(number),
            json!({ "n": number }).to_string().as_bytes(),
            PutDocumentOptions {
                metadata: BTreeMap::from([("source".into(), json!("first"))]),
            },
        )
        .unwrap();
}

fn fields(number: usize) -> Map<String, Value> {
    let Value::Object(fields) = json!({ "n": number }) else {
        unreachable!()
    };
    fields
}

#[test]
fn an_efs_put_commits_one_log_record_with_its_sidecar() {
    let fixture = Fixture::create();
    let writer = fixture.writer();
    put(&writer, 0);
    let before = fixture.log_records();
    put(&writer, 1);
    assert_eq!(fixture.state()["format"], "fylo.collection-generation.v2");
    assert_eq!(fixture.log_records(), before + 1);
    assert_eq!(fixture.read(&identifier(1)), json!({ "n": 1 }));
    assert_eq!(fixture.metadata(&identifier(1))["source"], json!("first"));
    assert!(sidecar(&fixture.document_path(&identifier(1))).is_file());
}

#[test]
fn recovery_replays_an_efs_payload_and_sidecar_whose_apply_was_lost() {
    let fixture = Fixture::create();
    let writer = fixture.writer();
    put(&writer, 2);
    let document = fixture.document_path(&identifier(2));
    let manifest = sidecar(&document);
    let bytes = fs::read(&document).unwrap();
    fs::remove_file(&document).unwrap();
    fs::remove_file(&manifest).unwrap();
    fixture.mark_writing();
    assert!(writer.recover_collection("items").unwrap());
    assert_eq!(fs::read(&document).unwrap(), bytes);
    assert!(manifest.is_file());
    assert_eq!(fixture.metadata(&identifier(2))["source"], json!("first"));
    assert!(!writer.recover_collection("items").unwrap());
}

#[test]
fn an_efs_batch_of_25_is_one_log_record() {
    let fixture = Fixture::create();
    let writer = fixture.writer();
    put(&writer, 0);
    let before = fixture.log_records();
    let batch: Vec<DocumentWrite> = (100..125)
        .map(|number| DocumentWrite::Put {
            identifier: identifier(number),
            fields: fields(number),
        })
        .collect();
    writer
        .commit_document_writes_with("items", &batch, &PlainDocumentCodec)
        .unwrap();
    assert_eq!(fixture.log_records(), before + 1);
    assert_eq!(fixture.read(&identifier(124)), json!({ "n": 124 }));
}

#[test]
fn efs_patch_metadata_delete_and_restore_round_trip_through_the_log() {
    let fixture = Fixture::create();
    let writer = fixture.writer();
    put(&writer, 3);
    let id = identifier(3);
    writer
        .patch_document("items", &id, br#"{"n":3,"extra":true}"#)
        .unwrap();
    assert_eq!(fixture.read(&id), json!({ "n": 3, "extra": true }));
    let Value::Object(record) = json!({ "source": "second" }) else {
        unreachable!()
    };
    writer.set_record_metadata("items", &id, &record).unwrap();
    assert_eq!(fixture.metadata(&id)["source"], json!("second"));
    writer.delete_document("items", &id).unwrap();
    assert!(
        fixture
            .reader()
            .collection("items")
            .unwrap()
            .read_document(&id)
            .is_err()
    );
    writer.restore_document("items", &id).unwrap();
    assert_eq!(fixture.read(&id), json!({ "n": 3, "extra": true }));
    assert_eq!(fixture.metadata(&id)["source"], json!("second"));
}

#[test]
fn an_efs_put_refuses_an_orphan_sidecar() {
    let fixture = Fixture::create();
    let writer = fixture.writer();
    put(&writer, 4);
    let shard = fixture.document_path(&identifier(4));
    let shard = shard.parent().unwrap();
    let width = u32::try_from(shard.file_name().unwrap().len()).unwrap();
    let target = shard
        .with_file_name(fylo_storage_native::shard_of(&identifier(5), width))
        .join(format!("{}.json", identifier(5)));
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(sidecar(&target), b"{}").unwrap();
    let error = writer
        .put_document(
            "items",
            &identifier(5),
            br#"{"n":5}"#,
            PutDocumentOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::CorruptMetadata);
}

#[test]
fn a_crash_between_a_document_and_its_sidecar_recovers_both() {
    let mut fixture = Fixture::create();
    put(&fixture.writer(), 6);
    let neighbour = fixture.document_path(&identifier(6));
    fixture.release_lease();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "efs_crash_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ROOT, &fixture.root)
        .env("FYLO_RUST_FAILPOINT", "after-log-apply-step")
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "the put did not stop after its first step"
    );
    fixture.take_lease();
    let shard = neighbour.parent().unwrap();
    let width = u32::try_from(shard.file_name().unwrap().len()).unwrap();
    let document = shard
        .with_file_name(fylo_storage_native::shard_of(&identifier(7), width))
        .join(format!("{}.json", identifier(7)));
    assert!(document.is_file());
    assert!(!sidecar(&document).exists());
    assert!(fixture.writer().recover_collection("items").unwrap());
    assert!(sidecar(&document).is_file());
    assert_eq!(fixture.read(&identifier(7)), json!({ "n": 7 }));
    assert_eq!(fixture.metadata(&identifier(7))["source"], json!("first"));
}

/// The other half of the crash test: does nothing in an ordinary run.
#[test]
fn efs_crash_child() {
    let Ok(root) = std::env::var(CHILD_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let writer = NativeWriteRoot::open_profiled(efs_options(&root, efs_lease(&root))).unwrap();
    put(&writer, 7);
}

#[test]
fn recovery_does_not_bring_back_an_efs_document_a_later_record_deleted() {
    let fixture = Fixture::create();
    let writer = fixture.writer();
    put(&writer, 8);
    let live = fixture.document_path(&identifier(8));
    writer.delete_document("items", &identifier(8)).unwrap();
    fixture.mark_writing();
    assert!(writer.recover_collection("items").unwrap());
    assert!(!live.exists());
    assert!(!sidecar(&live).exists());
    writer.restore_document("items", &identifier(8)).unwrap();
    assert_eq!(fixture.read(&identifier(8)), json!({ "n": 8 }));
}
