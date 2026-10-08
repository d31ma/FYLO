//! Public API contracts for a profiled root, its generation barrier, and access.
//! These are source-only contract tests until the native validation lane is admitted.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_engine::{AccessContext, EngineErrorCode, ReadOnlyEngine, WriteEngine};
use fylo_storage_native::{
    CollectionKind, FilesystemProfile, NativeRoot, NativeStorageErrorCode, NativeWriteRoot,
    PutDocumentOptions, PutRawFileOptions, RootLease, RootOpenOptions, WriteAccess, sidecar_path,
};
use serde_json::json;

const ID: &str = "4VRNF52JPCO";
const RAW_ID: &str = "4VRNF52JPCP";
const OTHER_RAW_ID: &str = "4VRNF52JPCQ";
const FIXTURE_KEY: &str = "fixture-key-at-least-thirty-two-bytes";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    lease: RootLease,
}

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-efs-engine-{}-{stamp}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = parent.join("root");
        fs::create_dir_all(&root).unwrap();
        let lease = RootLease::acquire_with_profile(&root, FilesystemProfile::EfsSidecarV1).unwrap();
        let fixture = Self { parent, root, lease };
        let writer = fixture.writer();
        writer.create_collection("records", CollectionKind::Document, None).unwrap();
        writer.create_collection("assets", CollectionKind::File, None).unwrap();
        writer.put_document("records", ID, br#"{"visibility":"public"}"#,
            PutDocumentOptions {
                metadata: BTreeMap::from([
                    ("visibility".into(), json!("public")),
                    ("copy".into(), json!("public")),
                    ("id".into(), json!("custom-id-must-not-replace-canonical-id")),
                ]),
                access: WriteAccess { mode: Some(0o644), ..WriteAccess::default() },
            }).unwrap();
        for (id, key, bytes, label) in [
            (RAW_ID, "/first.bin", &b"first-payload"[..], "first"),
            (OTHER_RAW_ID, "/second.bin", &b"different-second-payload"[..], "second"),
        ] {
            writer.put_raw_file("assets", id, bytes, &PutRawFileOptions {
                key: key.into(), extension: ".bin".into(),
                metadata: BTreeMap::from([("label".into(), json!(label))]),
                access: WriteAccess { mode: Some(0o644), ..WriteAccess::default() },
            }).unwrap();
        }
        fixture
    }

    fn options(&self) -> RootOpenOptions {
        RootOpenOptions::new(&self.root)
            .with_profile(FilesystemProfile::EfsSidecarV1)
            .with_lease(self.lease.clone())
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(self.options()).unwrap()
    }

    fn reader(&self) -> ReadOnlyEngine {
        ReadOnlyEngine::open_profiled(self.options()).unwrap()
    }

    fn document_path(&self) -> PathBuf {
        NativeRoot::open_profiled(self.options()).unwrap()
            .collection("records").unwrap().read_document(ID).unwrap().path
    }

    fn raw_path(&self, id: &str) -> PathBuf {
        NativeRoot::open_profiled(self.options()).unwrap()
            .collection("assets").unwrap().read_raw_file(id).unwrap().path
    }

    fn outsider(&self) -> AccessContext {
        AccessContext::new(fs::metadata(self.document_path()).unwrap().uid().wrapping_add(1).max(1), [])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

#[test]
fn profiled_reads_keep_payload_custom_canonical_and_access_together() {
    let fixture = Fixture::new();
    let engine = fixture.reader();
    let actor = fixture.outsider();
    let record = engine.get_as("records", ID, &actor).unwrap();
    assert_eq!(record.document.fields()["visibility"], "public");
    let metadata = engine.metadata_as("records", ID, &actor).unwrap();
    assert_eq!(metadata["visibility"], "public");
    assert_eq!(metadata["copy"], "public");
    assert_eq!(metadata["id"], ID);
    assert_eq!(metadata["mtime"], json!(record.metadata.mtime));
    let raw = engine.get_file_as("assets", RAW_ID, &actor).unwrap();
    assert_eq!(raw.bytes, b"first-payload");
    assert_eq!(raw.file.key, "/first.bin");
    assert_eq!(raw.file.content_length, raw.bytes.len() as u64);
    assert_eq!(raw.custom_metadata["label"], "first");
    let raw_metadata = engine.metadata_as("assets", RAW_ID, &actor).unwrap();
    assert_eq!(raw_metadata["id"], RAW_ID);
    assert_eq!(raw_metadata["label"], "first");
    assert_eq!(raw_metadata["checksumSHA256"], raw.file.checksum_sha256);
    assert_eq!(raw_metadata["key"], raw.file.key);
}

#[test]
fn stable_missing_and_corrupt_manifests_never_fall_back_to_public_access() {
    for corruption in ["missing", "malformed"] {
        let fixture = Fixture::new();
        let engine = fixture.reader();
        let actor = fixture.outsider();
        let writer = fixture.writer();
        writer.set_record_access("records", ID,
            WriteAccess { mode: Some(0o600), ..WriteAccess::default() }, None).unwrap();
        writer.set_record_access("assets", RAW_ID,
            WriteAccess { mode: Some(0o600), ..WriteAccess::default() }, None).unwrap();
        assert_eq!(engine.get_as("records", ID, &actor).unwrap_err().code(), EngineErrorCode::Access);
        for path in [fixture.document_path(), fixture.raw_path(RAW_ID)] {
            if corruption == "missing" {
                fs::remove_file(sidecar_path(&path)).unwrap();
            } else {
                fs::write(sidecar_path(&path), b"{invalid-json").unwrap();
            }
        }
        for result in [engine.metadata_as("records", ID, &actor), engine.metadata_as("assets", RAW_ID, &actor)] {
            assert_eq!(result.unwrap_err().storage_code(), Some(NativeStorageErrorCode::CorruptMetadata));
        }
        assert_eq!(engine.get_as("records", ID, &actor).unwrap_err().storage_code(), Some(NativeStorageErrorCode::CorruptMetadata));
        assert_eq!(engine.get_file_as("assets", RAW_ID, &actor).unwrap_err().storage_code(), Some(NativeStorageErrorCode::CorruptMetadata));
    }
}

#[test]
fn a_swapped_or_symlinked_sidecar_cannot_supply_another_payloads_identity() {
    for symlinked in [false, true] {
        let fixture = Fixture::new();
        let first = sidecar_path(&fixture.raw_path(RAW_ID));
        let second = sidecar_path(&fixture.raw_path(OTHER_RAW_ID));
        if symlinked {
            fs::remove_file(&first).unwrap();
            symlink(&second, &first).unwrap();
        } else {
            let held = first.with_extension("held");
            fs::rename(&first, &held).unwrap();
            fs::rename(&second, &first).unwrap();
            fs::rename(&held, &second).unwrap();
        }
        let engine = fixture.reader();
        let error = engine.get_file_as("assets", RAW_ID, &fixture.outsider()).unwrap_err();
        assert!(matches!(error.storage_code(),
            Some(NativeStorageErrorCode::CorruptMetadata | NativeStorageErrorCode::UnsafePath)), "{error}");
        assert!(engine.metadata_as("assets", RAW_ID, &fixture.outsider()).is_err());
    }
}

#[test]
fn a_writing_generation_blocks_metadata_and_payload_as_one_read_attempt() {
    let fixture = Fixture::new();
    let state = fixture.root.join(".fylo-transactions/.collections/records/state.json");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(state, br#"{"format":"fylo.collection-generation.v1","generation":1,"state":"writing","transactionId":"interrupted-fixture"}"#).unwrap();
    let engine = fixture.reader();
    let actor = fixture.outsider();
    assert_eq!(engine.get_as("records", ID, &actor).unwrap_err().code(), EngineErrorCode::ConcurrentWrite);
    assert_eq!(engine.metadata_as("records", ID, &actor).unwrap_err().code(), EngineErrorCode::ConcurrentWrite);
}

#[test]
fn revocation_never_exposes_private_payload_or_metadata_from_a_later_generation() {
    let fixture = Fixture::new();
    let actor_uid = fs::metadata(fixture.document_path()).unwrap().uid().wrapping_add(1).max(1);
    let barrier = Arc::new(Barrier::new(3));
    std::thread::scope(|scope| {
        let readers: Vec<_> = (0..2).map(|_| {
            let barrier = barrier.clone();
            let options = fixture.options();
            scope.spawn(move || {
                let opened = ReadOnlyEngine::open_profiled(options);
                barrier.wait();
                let engine = opened.unwrap();
                let actor = AccessContext::new(actor_uid, []);
                for _ in 0..600 {
                    match engine.get_as("records", ID, &actor) {
                        Ok(record) => assert_eq!(record.document.fields()["visibility"], "public"),
                        Err(error) => assert!(matches!(error.code(), EngineErrorCode::Access | EngineErrorCode::ConcurrentWrite), "{error}"),
                    }
                    for (collection, id) in [("records", ID), ("assets", RAW_ID)] {
                        match engine.metadata_as(collection, id, &actor) {
                            Ok(metadata) => {
                                assert_eq!(metadata["id"], id);
                                assert_eq!(metadata["visibility"], "public", "private metadata escaped: {collection}");
                                assert_eq!(metadata["copy"], "public");
                            }
                            Err(error) => assert!(matches!(error.code(), EngineErrorCode::Access | EngineErrorCode::ConcurrentWrite), "{error}"),
                        }
                    }
                    std::thread::yield_now();
                }
            })
        }).collect();
        let writer = fixture.writer();
        writer.replace_record_metadata("assets", RAW_ID, json!({"visibility":"public","copy":"public"}).as_object().unwrap(), None).unwrap();
        barrier.wait();
        for _ in 0..40 {
            for (collection, id) in [("records", ID), ("assets", RAW_ID)] {
                writer.set_record_access(collection, id, WriteAccess { mode: Some(0o600), ..WriteAccess::default() }, None).unwrap();
                if collection == "records" {
                    writer.patch_document(collection, id, br#"{"visibility":"private"}"#, None).unwrap();
                }
                writer.replace_record_metadata(collection, id, json!({"visibility":"private","copy":"private"}).as_object().unwrap(), None).unwrap();
                std::thread::yield_now();
                if collection == "records" {
                    writer.patch_document(collection, id, br#"{"visibility":"public"}"#, None).unwrap();
                }
                writer.replace_record_metadata(collection, id, json!({"visibility":"public","copy":"public"}).as_object().unwrap(), None).unwrap();
                writer.set_record_access(collection, id, WriteAccess { mode: Some(0o644), ..WriteAccess::default() }, None).unwrap();
            }
        }
        for reader in readers { reader.join().unwrap(); }
    });
    let engine = fixture.reader();
    let actor = fixture.outsider();
    assert_eq!(engine.get_as("records", ID, &actor).unwrap().document.fields()["visibility"], "public");
    assert_eq!(engine.metadata_as("records", ID, &actor).unwrap()["visibility"], "public");
    assert_eq!(engine.metadata_as("assets", RAW_ID, &actor).unwrap()["visibility"], "public");
}

#[test]
fn native_profile_preserves_existing_config_schema_encryption_and_ciphertext_denial() {
    let fixture = Fixture::new();
    let native = fixture.parent.join("native");
    let schema = fixture.parent.join("schemas");
    fs::create_dir_all(&native).unwrap();
    fs::create_dir_all(schema.join("records/history")).unwrap();
    fs::write(schema.join("records/manifest.json"), br#"{"current":"v1","versions":[{"v":"v1"}]}"#).unwrap();
    fs::write(schema.join("records/history/v1.schema.json"), br#"{"$encrypted":["secret"]}"#).unwrap();
    let config = fylo_storage_native::RootConfig { strict_schema: false, shard_width: Some(2) };
    assert_eq!(WriteEngine::open_profiled(RootOpenOptions::new(&native)).unwrap().with_config(config).config(), config);
    NativeWriteRoot::open(&native).unwrap().create_collection("records", CollectionKind::Document, None).unwrap();
    // Synthetic credentials exist only in this disposable fixture.
    WriteEngine::open_with_encryption(&native, &schema, FIXTURE_KEY, "fixture-salt").unwrap()
        .put_document("records", ID, json!({"secret":"fixture-plaintext","name":"Ada"}).as_object().unwrap().clone(), WriteAccess::default()).unwrap();
    let encrypted = NativeRoot::open(&native).unwrap().collection("records").unwrap().read_document(ID).unwrap();
    assert!(!String::from_utf8_lossy(&encrypted.bytes).contains("fixture-plaintext"));
    let legacy = ReadOnlyEngine::open_with_encryption(&native, &schema, FIXTURE_KEY, "fixture-salt").unwrap().get("records", ID).unwrap();
    let profiled = ReadOnlyEngine::open_profiled(RootOpenOptions::new(&native)).unwrap()
        .with_schema_configuration(&schema, Some((FIXTURE_KEY, "fixture-salt"))).unwrap().get("records", ID).unwrap();
    assert_eq!(profiled.document.fields(), legacy.document.fields());
    assert_eq!(profiled.metadata.id, legacy.metadata.id);
    assert_eq!(profiled.metadata.mtime, legacy.metadata.mtime);
    let error = ReadOnlyEngine::open_profiled(RootOpenOptions::new(&native)).unwrap().get("records", ID).unwrap_err();
    assert_eq!(error.code(), EngineErrorCode::Encryption);
    assert!(!error.to_string().contains("fixture-plaintext"));
    assert!(!native.join(".fylo-catalog/filesystem-profile.json").exists());
}
