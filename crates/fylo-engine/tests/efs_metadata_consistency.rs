//! Public API contracts for a profiled root and its generation barrier.
//! These are source-only contract tests until the native validation lane is admitted.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_engine::{EngineErrorCode, ReadOnlyEngine, WriteEngine};
use fylo_storage_native::{
    CollectionKind, FilesystemProfile, NativeRoot, NativeStorageErrorCode, NativeWriteRoot,
    PutDocumentOptions, PutRawFileOptions, RootLease, RootOpenOptions, sidecar_path,
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
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-efs-engine-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = parent.join("root");
        fs::create_dir_all(&root).unwrap();
        let lease =
            RootLease::acquire_with_profile(&root, FilesystemProfile::EfsSidecarV1).unwrap();
        let fixture = Self {
            parent,
            root,
            lease,
        };
        let writer = fixture.writer();
        writer
            .create_collection("records", CollectionKind::Document, None)
            .unwrap();
        writer
            .create_collection("assets", CollectionKind::File, None)
            .unwrap();
        writer
            .put_document(
                "records",
                ID,
                br#"{"visibility":"public"}"#,
                PutDocumentOptions {
                    metadata: BTreeMap::from([
                        ("visibility".into(), json!("public")),
                        ("copy".into(), json!("public")),
                        (
                            "id".into(),
                            json!("custom-id-must-not-replace-canonical-id"),
                        ),
                    ]),
                },
            )
            .unwrap();
        for (id, key, bytes, label) in [
            (RAW_ID, "/first.bin", &b"first-payload"[..], "first"),
            (
                OTHER_RAW_ID,
                "/second.bin",
                &b"different-second-payload"[..],
                "second",
            ),
        ] {
            writer
                .put_raw_file(
                    "assets",
                    id,
                    bytes,
                    &PutRawFileOptions {
                        key: key.into(),
                        extension: ".bin".into(),
                        metadata: BTreeMap::from([("label".into(), json!(label))]),
                    },
                )
                .unwrap();
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
        NativeRoot::open_profiled(self.options())
            .unwrap()
            .collection("records")
            .unwrap()
            .read_document(ID)
            .unwrap()
            .path
    }

    fn raw_path(&self, id: &str) -> PathBuf {
        NativeRoot::open_profiled(self.options())
            .unwrap()
            .collection("assets")
            .unwrap()
            .read_raw_file(id)
            .unwrap()
            .path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

#[test]
fn a_completed_same_payload_manifest_swap_makes_both_records_corrupt() {
    let fixture = Fixture::new();
    let engine = fixture.reader();
    let first = fixture.document_path();
    let bytes = fs::read(&first).unwrap();
    fixture
        .writer()
        .put_document(
            "records",
            OTHER_RAW_ID,
            &bytes,
            PutDocumentOptions {
                metadata: BTreeMap::from([("copy".into(), json!("private-metadata"))]),
            },
        )
        .unwrap();
    let second = NativeRoot::open_profiled(fixture.options())
        .unwrap()
        .collection("records")
        .unwrap()
        .read_document(OTHER_RAW_ID)
        .unwrap()
        .path;
    assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    let first_manifest = fs::read(sidecar_path(&first)).unwrap();
    let second_manifest = fs::read(sidecar_path(&second)).unwrap();
    fs::write(sidecar_path(&first), &second_manifest).unwrap();
    fs::write(sidecar_path(&second), &first_manifest).unwrap();
    for id in [ID, OTHER_RAW_ID] {
        assert_eq!(
            engine.get("records", id).unwrap_err().storage_code(),
            Some(NativeStorageErrorCode::CorruptMetadata)
        );
        assert_eq!(
            engine.metadata("records", id).unwrap_err().storage_code(),
            Some(NativeStorageErrorCode::CorruptMetadata)
        );
    }
    assert_eq!(fs::read(sidecar_path(&first)).unwrap(), second_manifest);
    assert_eq!(fs::read(sidecar_path(&second)).unwrap(), first_manifest);
}

#[test]
fn profiled_reads_keep_payload_custom_and_canonical_metadata_together() {
    let fixture = Fixture::new();
    let engine = fixture.reader();
    let record = engine.get("records", ID).unwrap();
    assert_eq!(record.document.fields()["visibility"], "public");
    let metadata = engine.metadata("records", ID).unwrap();
    assert_eq!(metadata["visibility"], "public");
    assert_eq!(metadata["copy"], "public");
    assert_eq!(metadata["id"], ID);
    assert_eq!(metadata["mtime"], json!(record.metadata.mtime));
    let raw = engine.get_file("assets", RAW_ID).unwrap();
    assert_eq!(raw.bytes, b"first-payload");
    assert_eq!(raw.file.key, "/first.bin");
    assert_eq!(raw.file.content_length, raw.bytes.len() as u64);
    assert_eq!(raw.custom_metadata["label"], "first");
    let raw_metadata = engine.metadata("assets", RAW_ID).unwrap();
    assert_eq!(raw_metadata["id"], RAW_ID);
    assert_eq!(raw_metadata["label"], "first");
    assert_eq!(raw_metadata["checksumSHA256"], raw.file.checksum_sha256);
    assert_eq!(raw_metadata["key"], raw.file.key);
}

#[test]
fn raw_descriptor_fields_override_colliding_custom_metadata_inside_the_envelope() {
    let fixture = Fixture::new();
    fixture
        .writer()
        .set_record_metadata(
            "assets",
            RAW_ID,
            json!({
                "id": "spoofed-id",
                "createdAt": 0,
                "updatedAt": 0,
                "mtime": 0,
                "name": "spoofed-name",
                "key": "/spoofed-key",
                "extension": ".txt",
                "contentType": "text/plain",
                "contentLength": 0,
                "etag": "spoofed-etag",
                "checksumSHA256": "spoofed-checksum",
                "lastModified": 0,
                "label": "preserved-custom-value",
            })
            .as_object()
            .unwrap(),
        )
        .unwrap();
    let engine = fixture.reader();
    let raw = engine.get_file("assets", RAW_ID).unwrap();
    assert_eq!(raw.bytes, b"first-payload");
    assert_eq!(raw.custom_metadata["key"], "/spoofed-key");
    let metadata = engine.metadata("assets", RAW_ID).unwrap();
    for (name, value) in serde_json::to_value(&raw.file)
        .unwrap()
        .as_object()
        .unwrap()
    {
        assert_eq!(
            &metadata[name], value,
            "custom metadata replaced raw descriptor field {name}"
        );
    }
    for (name, value) in serde_json::to_value(&raw.metadata)
        .unwrap()
        .as_object()
        .unwrap()
    {
        assert_eq!(
            &metadata[name], value,
            "custom metadata replaced canonical field {name}"
        );
    }
    assert_eq!(metadata["label"], "preserved-custom-value");
}

#[test]
fn stable_missing_and_corrupt_manifests_fail_closed() {
    for corruption in ["missing", "malformed"] {
        let fixture = Fixture::new();
        let engine = fixture.reader();
        for path in [fixture.document_path(), fixture.raw_path(RAW_ID)] {
            if corruption == "missing" {
                fs::remove_file(sidecar_path(&path)).unwrap();
            } else {
                fs::write(sidecar_path(&path), b"{invalid-json").unwrap();
            }
        }
        for result in [
            engine.metadata("records", ID),
            engine.metadata("assets", RAW_ID),
        ] {
            assert_eq!(
                result.unwrap_err().storage_code(),
                Some(NativeStorageErrorCode::CorruptMetadata)
            );
        }
        assert_eq!(
            engine.get("records", ID).unwrap_err().storage_code(),
            Some(NativeStorageErrorCode::CorruptMetadata)
        );
        assert_eq!(
            engine
                .get_file("assets", RAW_ID)
                .unwrap_err()
                .storage_code(),
            Some(NativeStorageErrorCode::CorruptMetadata)
        );
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
        let error = engine.get_file("assets", RAW_ID).unwrap_err();
        assert!(
            matches!(
                error.storage_code(),
                Some(NativeStorageErrorCode::CorruptMetadata | NativeStorageErrorCode::UnsafePath)
            ),
            "{error}"
        );
        assert!(engine.metadata("assets", RAW_ID).is_err());
    }
}

#[test]
fn a_writing_generation_blocks_metadata_and_payload_as_one_read_attempt() {
    let fixture = Fixture::new();
    let state = fixture
        .root
        .join(".fylo-transactions/.collections/records/state.json");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(state, br#"{"format":"fylo.collection-generation.v1","generation":1,"state":"writing","transactionId":"interrupted-fixture"}"#).unwrap();
    let engine = fixture.reader();
    assert_eq!(
        engine.get("records", ID).unwrap_err().code(),
        EngineErrorCode::ConcurrentWrite
    );
    assert_eq!(
        engine.metadata("records", ID).unwrap_err().code(),
        EngineErrorCode::ConcurrentWrite
    );
}

#[test]
fn native_profile_preserves_existing_config_schema_encryption_and_ciphertext_denial() {
    let fixture = Fixture::new();
    let native = fixture.parent.join("native");
    let schema = fixture.parent.join("schemas");
    fs::create_dir_all(&native).unwrap();
    fs::create_dir_all(schema.join("records/history")).unwrap();
    fs::write(
        schema.join("records/manifest.json"),
        br#"{"current":"v1","versions":[{"v":"v1"}]}"#,
    )
    .unwrap();
    fs::write(
        schema.join("records/history/v1.schema.json"),
        br#"{"$encrypted":["secret"]}"#,
    )
    .unwrap();
    let config = fylo_storage_native::RootConfig {
        strict_schema: false,
        shard_width: Some(2),
        ..Default::default()
    };
    assert_eq!(
        WriteEngine::open_profiled(RootOpenOptions::new(&native))
            .unwrap()
            .with_config(config)
            .config(),
        config
    );
    NativeWriteRoot::open(&native)
        .unwrap()
        .create_collection("records", CollectionKind::Document, None)
        .unwrap();
    // Synthetic credentials exist only in this disposable fixture.
    WriteEngine::open_with_encryption(&native, &schema, FIXTURE_KEY, "fixture-salt")
        .unwrap()
        .put_document(
            "records",
            ID,
            json!({"secret":"fixture-plaintext","name":"Ada"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let encrypted = NativeRoot::open(&native)
        .unwrap()
        .collection("records")
        .unwrap()
        .read_document(ID)
        .unwrap();
    assert!(!String::from_utf8_lossy(&encrypted.bytes).contains("fixture-plaintext"));
    let legacy =
        ReadOnlyEngine::open_with_encryption(&native, &schema, FIXTURE_KEY, "fixture-salt")
            .unwrap()
            .get("records", ID)
            .unwrap();
    let profiled = ReadOnlyEngine::open_profiled(RootOpenOptions::new(&native))
        .unwrap()
        .with_schema_configuration(&schema, Some((FIXTURE_KEY, "fixture-salt")))
        .unwrap()
        .get("records", ID)
        .unwrap();
    assert_eq!(profiled.document.fields(), legacy.document.fields());
    assert_eq!(profiled.metadata.id, legacy.metadata.id);
    assert_eq!(profiled.metadata.mtime, legacy.metadata.mtime);
    let error = ReadOnlyEngine::open_profiled(RootOpenOptions::new(&native))
        .unwrap()
        .get("records", ID)
        .unwrap_err();
    assert_eq!(error.code(), EngineErrorCode::Encryption);
    assert!(!error.to_string().contains("fixture-plaintext"));
    assert!(
        !native
            .join(".fylo-catalog/filesystem-profile.json")
            .exists()
    );
}
