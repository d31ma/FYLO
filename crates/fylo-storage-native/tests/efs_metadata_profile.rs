//! Proposed explicit EFS metadata contract. Run only in the separately admitted
//! native validation lane; local success would not qualify Lambda or NFS/EFS.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fylo_storage_native::{
    CollectionKind, FilesystemProfile, NativeRoot, NativeStorageErrorCode, NativeWriteRoot,
    PutDocumentOptions, PutRawFileOptions, RootLease, RootOpenOptions, WriteAccess, WriteActor,
};
use serde_json::{Value, json};

const ID: &str = "4VRNF52JPCO";
const RAW_ID: &str = "4VRNF52JPCP";
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    lease: RootLease,
}

impl Fixture {
    fn empty() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // Canonical so engine-reported paths share its prefix: macOS hands
        // out temp dirs under the /var -> /private/var symlink.
        let parent = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "fylo-efs-profile-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let root = parent.join("root");
        fs::create_dir_all(&root).unwrap();
        let lease =
            RootLease::acquire_with_profile(&root, FilesystemProfile::EfsSidecarV1).unwrap();
        assert!(
            lease.kernel_enforced(),
            "profile needs an enforced kernel lease"
        );
        Self {
            parent,
            root,
            lease,
        }
    }

    fn options(&self) -> RootOpenOptions {
        RootOpenOptions::new(&self.root)
            .with_profile(FilesystemProfile::EfsSidecarV1)
            .with_lease(self.lease.clone())
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(self.options()).unwrap()
    }

    fn actor(&self) -> WriteActor {
        let owner = fs::metadata(&self.root).unwrap();
        WriteActor::new(owner.uid(), [owner.gid()])
    }

    fn reader(&self) -> NativeRoot {
        NativeRoot::open_profiled(self.options()).unwrap()
    }

    fn seed(&self) -> NativeWriteRoot {
        let writer = self.writer();
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
                br#"{"name":"Ada","score":42}"#,
                PutDocumentOptions {
                    metadata: BTreeMap::from([
                        ("reviewer".into(), json!("before")),
                        ("typed".into(), json!({"accepted": true, "count": 2})),
                    ]),
                    access: WriteAccess {
                        mode: Some(0o640),
                        ..WriteAccess::default()
                    },
                },
            )
            .unwrap();
        writer
            .put_raw_file(
                "assets",
                RAW_ID,
                &[0, 1, 2, 255],
                &PutRawFileOptions {
                    key: "/seed.bin".into(),
                    extension: ".bin".into(),
                    metadata: BTreeMap::from([("source".into(), json!("seed"))]),
                    access: WriteAccess {
                        mode: Some(0o640),
                        ..WriteAccess::default()
                    },
                },
            )
            .unwrap();
        writer
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The sibling lease files remain inside this disposable test parent.
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn sidecar(payload: &Path) -> PathBuf {
    let mut name = payload.as_os_str().to_os_string();
    name.push(".fylo-attrs");
    PathBuf::from(name)
}

fn attributes(payload: &Path) -> BTreeMap<String, String> {
    serde_json::from_slice(&fs::read(sidecar(payload)).unwrap()).unwrap()
}

fn swap_manifests(first: &Path, second: &Path) {
    let first_bytes = fs::read(sidecar(first)).unwrap();
    let second_bytes = fs::read(sidecar(second)).unwrap();
    fs::write(sidecar(first), second_bytes).unwrap();
    fs::write(sidecar(second), first_bytes).unwrap();
}

#[test]
fn identical_document_payloads_cannot_exchange_record_manifests() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let first = records.read_document(ID).unwrap();
    writer
        .put_document(
            "records",
            RAW_ID,
            &first.bytes,
            PutDocumentOptions {
                metadata: BTreeMap::from([("reviewer".into(), json!("different-record"))]),
                access: WriteAccess {
                    mode: Some(0o600),
                    ..WriteAccess::default()
                },
            },
        )
        .unwrap();
    let second = records.read_document(RAW_ID).unwrap();
    assert_eq!(first.bytes, second.bytes);
    swap_manifests(&first.path, &second.path);
    for id in [ID, RAW_ID] {
        assert_eq!(
            records.read_document(id).unwrap_err().code(),
            NativeStorageErrorCode::CorruptMetadata
        );
        assert_eq!(
            records.read_custom_metadata(id).unwrap_err().code(),
            NativeStorageErrorCode::CorruptMetadata
        );
    }
}

#[test]
fn the_same_document_id_in_another_collection_cannot_supply_its_manifest() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let first = records.read_document(ID).unwrap();
    writer
        .create_collection("other-records", CollectionKind::Document, None)
        .unwrap();
    writer
        .put_document(
            "other-records",
            ID,
            &first.bytes,
            PutDocumentOptions {
                metadata: BTreeMap::from([("reviewer".into(), json!("different-collection"))]),
                access: WriteAccess {
                    mode: Some(0o600),
                    ..WriteAccess::default()
                },
            },
        )
        .unwrap();
    let other = fixture.reader().collection("other-records").unwrap();
    let second = other.read_document(ID).unwrap();
    assert_eq!(first.bytes, second.bytes);
    swap_manifests(&first.path, &second.path);
    assert_eq!(
        records.read_document(ID).unwrap_err().code(),
        NativeStorageErrorCode::CorruptMetadata
    );
    assert_eq!(
        other.read_document(ID).unwrap_err().code(),
        NativeStorageErrorCode::CorruptMetadata
    );
}

#[test]
fn a_document_payload_change_without_its_bound_manifest_is_corrupt() {
    let fixture = Fixture::empty();
    let _writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let first = records.read_document(ID).unwrap();
    let manifest = fs::read(sidecar(&first.path)).unwrap();
    fs::write(&first.path, br#"{"name":"other payload","score":42}"#).unwrap();
    assert_eq!(
        records.read_document(ID).unwrap_err().code(),
        NativeStorageErrorCode::CorruptMetadata
    );
    assert_eq!(fs::read(sidecar(&first.path)).unwrap(), manifest);
}

#[test]
fn identical_raw_payloads_with_equal_times_cannot_exchange_manifests() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let assets = fixture.reader().collection("assets").unwrap();
    let first = assets.read_raw_file(RAW_ID).unwrap();
    let other_id = "4VRNF52JPCQ";
    writer
        .put_raw_file(
            "assets",
            other_id,
            &first.bytes,
            &PutRawFileOptions {
                key: "/other-record.bin".into(),
                extension: ".bin".into(),
                metadata: BTreeMap::from([("source".into(), json!("different-record"))]),
                access: WriteAccess {
                    mode: Some(0o600),
                    ..WriteAccess::default()
                },
            },
        )
        .unwrap();
    let second = assets.read_raw_file(other_id).unwrap();
    fs::File::open(&second.path)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(fs::metadata(&first.path).unwrap().modified().unwrap()),
        )
        .unwrap();
    let mut second_attributes = attributes(&second.path);
    second_attributes.insert(
        "user.fylo.checksum".into(),
        attributes(&first.path)["user.fylo.checksum"].clone(),
    );
    fs::write(
        sidecar(&second.path),
        serde_json::to_vec(&second_attributes).unwrap(),
    )
    .unwrap();
    assert_eq!(
        assets.read_raw_file(other_id).unwrap().modified_millis,
        first.modified_millis
    );
    assert_eq!(first.bytes, second.bytes);
    swap_manifests(&first.path, &second.path);
    for id in [RAW_ID, other_id] {
        assert_eq!(
            assets.read_raw_file(id).unwrap_err().code(),
            NativeStorageErrorCode::CorruptMetadata
        );
    }
}

#[test]
fn an_unbound_efs_manifest_cannot_be_adopted_or_laundered_by_metadata_writes() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let path = records.read_document(ID).unwrap().path;
    let payload = fs::read(&path).unwrap();
    let mut unbound = attributes(&path);
    unbound.remove("user.fylo.record-identity");
    let unbound = serde_json::to_vec(&unbound).unwrap();
    fs::write(sidecar(&path), &unbound).unwrap();
    assert_eq!(
        records.read_document(ID).unwrap_err().code(),
        NativeStorageErrorCode::CorruptMetadata
    );
    assert!(
        writer
            .set_record_metadata(
                "records",
                ID,
                json!({"reviewer":"cannot repair identity"})
                    .as_object()
                    .unwrap(),
                Some(&fixture.actor())
            )
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), payload);
    assert_eq!(fs::read(sidecar(&path)).unwrap(), unbound);
}

fn assert_private_sidecar(payload: &Path) {
    let manifest = fs::symlink_metadata(sidecar(payload)).unwrap();
    assert!(manifest.file_type().is_file());
    assert_eq!(manifest.permissions().mode() & 0o777, 0o600);
    assert_eq!(manifest.uid(), fs::metadata(payload).unwrap().uid());
    assert_eq!(manifest.gid(), fs::metadata(payload).unwrap().gid());
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32, u32, u32, SystemTime)> {
    fn visit(
        root: &Path,
        path: &Path,
        entries: &mut BTreeMap<PathBuf, (Vec<u8>, u32, u32, u32, SystemTime)>,
    ) {
        let metadata = fs::symlink_metadata(path).unwrap();
        entries.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            (
                if metadata.is_file() {
                    fs::read(path).unwrap()
                } else {
                    Vec::new()
                },
                metadata.mode(),
                metadata.uid(),
                metadata.gid(),
                if metadata.is_file() {
                    metadata.modified().unwrap()
                } else {
                    UNIX_EPOCH
                },
            ),
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn stopped_pair_transaction(fixture: &Fixture, payload: &Path) -> PathBuf {
    let collection = fixture.root.join(".collections/records");
    let state = fixture
        .root
        .join(".fylo-transactions/.collections/records/state.json");
    let generation: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    let prior = generation["generation"].as_u64().unwrap();
    let transaction = state.parent().unwrap().join("bound-recovery-fixture");
    fs::create_dir_all(transaction.join("before")).unwrap();
    fs::create_dir_all(transaction.join("captures")).unwrap();
    for (index, original) in [sidecar(payload), payload.to_owned()].iter().enumerate() {
        let metadata = fs::metadata(original).unwrap();
        let modified = metadata.modified().unwrap();
        let backup = format!("before/{index:06}.bin");
        let target = transaction.join(&backup);
        fs::copy(original, &target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        fs::File::open(&target)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let capture = json!({
            "path": original.strip_prefix(&collection).unwrap().to_str().unwrap(),
            "present": true, "backup": backup,
            "mode": metadata.mode() & 0o777,
            "mtimeMs": modified.duration_since(UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0,
            "xattrs": []
        });
        fs::write(
            transaction.join(format!("captures/{index:06}.json")),
            serde_json::to_vec(&capture).unwrap(),
        )
        .unwrap();
    }
    fs::write(
        transaction.join("transaction.json"),
        serde_json::to_vec(&json!({
            "format": "fylo.collection-transaction.v1", "id": "bound-recovery-fixture",
            "collection": "records", "operation": "patch-document", "phase": "active",
            "generationBefore": prior, "eventOffset": 0, "captures": []
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        state,
        serde_json::to_vec(&json!({
            "format": "fylo.collection-generation.v1", "generation": prior + 1,
            "state": "writing", "transactionId": "bound-recovery-fixture"
        }))
        .unwrap(),
    )
    .unwrap();
    transaction
}

#[test]
fn bound_roots_publish_v2_and_refuse_v1_without_adopting_data() {
    let fixture = Fixture::empty();
    let _writer = fixture.seed();
    let marker = fixture.root.join(".fylo-catalog/filesystem-profile.json");
    let mut descriptor: Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    assert_eq!(descriptor["format"], "fylo.filesystem-profile.v2");
    descriptor["format"] = json!("fylo.filesystem-profile.v1");
    fs::write(&marker, serde_json::to_vec(&descriptor).unwrap()).unwrap();
    let before = snapshot(&fixture.parent);
    assert_eq!(
        RootLease::acquire_with_profile(&fixture.root, FilesystemProfile::EfsSidecarV1)
            .err()
            .unwrap()
            .code(),
        NativeStorageErrorCode::Unsupported
    );
    assert_eq!(
        NativeRoot::open_profiled(fixture.options())
            .err()
            .unwrap()
            .code(),
        NativeStorageErrorCode::Unsupported
    );
    assert_eq!(
        NativeWriteRoot::open_profiled(fixture.options())
            .err()
            .unwrap()
            .code(),
        NativeStorageErrorCode::Unsupported
    );
    assert_eq!(snapshot(&fixture.parent), before);
}

#[test]
fn every_binding_field_and_encoding_must_match_before_metadata_exposure() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let path = records.read_document(ID).unwrap().path;
    let original = attributes(&path);
    let binding: Value = serde_json::from_slice(
        &BASE64
            .decode(&original["user.fylo.record-identity"])
            .unwrap(),
    )
    .unwrap();
    assert_eq!(binding[0], "fylo.record-identity.v1");
    assert_eq!(binding[1], ".collections");
    assert_eq!(binding[2], "records");
    assert_eq!(binding[3], ID);
    let mut corrupt_values = vec![b"null".to_vec(), b"[]".to_vec()];
    for (index, replacement) in [
        json!("unknown"),
        json!(".buckets"),
        json!("other_records"),
        json!(RAW_ID),
        json!("0".repeat(64)),
    ]
    .into_iter()
    .enumerate()
    {
        let mut changed = binding.clone();
        changed[index] = replacement;
        corrupt_values.push(serde_json::to_vec(&changed).unwrap());
    }
    corrupt_values.push(serde_json::to_vec_pretty(&binding).unwrap());
    for value in corrupt_values {
        let mut changed = original.clone();
        changed.insert("user.fylo.record-identity".into(), BASE64.encode(value));
        fs::write(sidecar(&path), serde_json::to_vec(&changed).unwrap()).unwrap();
        let before = snapshot(&fixture.root);
        assert_eq!(
            records.read_document(ID).unwrap_err().code(),
            NativeStorageErrorCode::CorruptMetadata
        );
        assert_eq!(
            records.read_custom_metadata(ID).unwrap_err().code(),
            NativeStorageErrorCode::CorruptMetadata
        );
        assert!(
            writer
                .replace_record_metadata(
                    "records",
                    ID,
                    json!({}).as_object().unwrap(),
                    Some(&fixture.actor())
                )
                .is_err()
        );
        assert_eq!(snapshot(&fixture.root), before);
    }
}

#[test]
fn payload_updates_refresh_binding_and_user_attribute_removal_retains_it() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let path = records.read_document(ID).unwrap().path;
    let before = attributes(&path)["user.fylo.record-identity"].clone();
    writer
        .patch_document(
            "records",
            ID,
            br#"{"name":"after","score":43}"#,
            Some(&fixture.actor()),
        )
        .unwrap();
    let after = records.read_document(ID).unwrap();
    assert_ne!(attributes(&after.path)["user.fylo.record-identity"], before);
    let patched = attributes(&after.path)["user.fylo.record-identity"].clone();
    writer
        .patch_document_fields(
            "records",
            ID,
            json!({"score": 44}).as_object().unwrap(),
            Some(&fixture.actor()),
        )
        .unwrap();
    let field_patched = attributes(&after.path)["user.fylo.record-identity"].clone();
    assert_ne!(field_patched, patched);
    assert_eq!(
        writer
            .execute_sql_mutation(
                "UPDATE records SET score = 45 WHERE score = 44",
                Some(&fixture.actor()),
                WriteAccess::default()
            )
            .unwrap()
            .affected,
        1
    );
    assert_ne!(
        attributes(&after.path)["user.fylo.record-identity"],
        field_patched
    );
    writer
        .replace_record_metadata(
            "records",
            ID,
            json!({}).as_object().unwrap(),
            Some(&fixture.actor()),
        )
        .unwrap();
    assert!(records.read_custom_metadata(ID).unwrap().is_empty());
    assert!(attributes(&after.path).contains_key("user.fylo.record-identity"));
    assert_eq!(
        serde_json::from_slice::<Value>(&records.read_document(ID).unwrap().bytes).unwrap()["score"],
        45
    );
}

#[test]
fn verified_before_images_restore_the_bound_pair_and_recover_idempotently() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let before = records.read_document(ID).unwrap();
    let manifest = fs::read(sidecar(&before.path)).unwrap();
    stopped_pair_transaction(&fixture, &before.path);
    fs::write(&before.path, b"incomplete mutation").unwrap();
    fs::write(sidecar(&before.path), b"{}").unwrap();
    writer.recover_collection("records").unwrap();
    assert_eq!(records.read_document(ID).unwrap().bytes, before.bytes);
    assert_eq!(fs::read(sidecar(&before.path)).unwrap(), manifest);
    let recovered = snapshot(&fixture.root);
    writer.recover_collection("records").unwrap();
    assert_eq!(snapshot(&fixture.root), recovered);
}

#[test]
fn corrupt_or_unbound_before_images_refuse_recovery_before_any_target_mutation() {
    for missing_binding in [false, true] {
        let fixture = Fixture::empty();
        let writer = fixture.seed();
        let path = fixture
            .reader()
            .collection("records")
            .unwrap()
            .read_document(ID)
            .unwrap()
            .path;
        let transaction = stopped_pair_transaction(&fixture, &path);
        let manifest = transaction.join("before/000000.bin");
        let mut values: BTreeMap<String, String> =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        if missing_binding {
            values.remove("user.fylo.record-identity");
        } else {
            let mut binding: Value = serde_json::from_slice(
                &BASE64.decode(&values["user.fylo.record-identity"]).unwrap(),
            )
            .unwrap();
            binding[3] = json!(RAW_ID);
            values.insert(
                "user.fylo.record-identity".into(),
                BASE64.encode(serde_json::to_vec(&binding).unwrap()),
            );
        }
        let modified = fs::metadata(&manifest).unwrap().modified().unwrap();
        fs::write(&manifest, serde_json::to_vec(&values).unwrap()).unwrap();
        fs::File::open(&manifest)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let before = snapshot(&fixture.root);
        assert_eq!(
            writer.recover_collection("records").unwrap_err().code(),
            NativeStorageErrorCode::CorruptMetadata
        );
        assert_eq!(snapshot(&fixture.root), before);
    }
}

#[test]
fn custom_metadata_limits_and_binding_name_collisions_remain_user_scoped() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let path = records.read_document(ID).unwrap().path;
    let binding = attributes(&path)["user.fylo.record-identity"].clone();
    let limit = "x".repeat(60 * 1024 - 2);
    writer
        .replace_record_metadata(
            "records",
            ID,
            json!({
                "limit": limit, "record-identity": "ordinary custom value"
            })
            .as_object()
            .unwrap(),
            Some(&fixture.actor()),
        )
        .unwrap();
    assert_eq!(
        records.read_custom_metadata(ID).unwrap()["record-identity"],
        "ordinary custom value"
    );
    assert_eq!(attributes(&path)["user.fylo.record-identity"], binding);
    let before = snapshot(&fixture.root);
    assert_eq!(
        writer
            .set_record_metadata(
                "records",
                ID,
                json!({
                    "too_large": "x".repeat(60 * 1024 - 1)
                })
                .as_object()
                .unwrap(),
                Some(&fixture.actor())
            )
            .unwrap_err()
            .code(),
        NativeStorageErrorCode::FileTooLarge
    );
    assert_eq!(snapshot(&fixture.root), before);
}

#[test]
fn a_record_without_user_or_access_attributes_still_has_a_bound_manifest() {
    let fixture = Fixture::empty();
    let writer = fixture.writer();
    writer
        .create_collection("records", CollectionKind::Document, None)
        .unwrap();
    writer
        .put_document(
            "records",
            ID,
            br#"{"value":1}"#,
            PutDocumentOptions::default(),
        )
        .unwrap();
    let records = fixture.reader().collection("records").unwrap();
    let path = records.read_document(ID).unwrap().path;
    let manifest = attributes(&path);
    assert_eq!(manifest.len(), 1);
    assert!(manifest.contains_key("user.fylo.record-identity"));
    assert!(records.read_custom_metadata(ID).unwrap().is_empty());
    writer
        .commit_if_dirty("binding-only metadata must persist in history")
        .unwrap()
        .unwrap();
}

#[test]
fn unbound_materialization_backup_cannot_replace_the_current_worktree() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    writer.commit_if_dirty("bound baseline").unwrap().unwrap();
    let path = fixture
        .reader()
        .collection("records")
        .unwrap()
        .read_document(ID)
        .unwrap()
        .path;
    let transaction = fixture.root.join(".fylo-vcs/staging/binding-recovery");
    let backup = transaction
        .join("previous")
        .join(path.strip_prefix(&fixture.root).unwrap());
    fs::create_dir_all(backup.parent().unwrap()).unwrap();
    fs::copy(&path, &backup).unwrap();
    let mut unbound = attributes(&path);
    unbound.remove("user.fylo.record-identity");
    fs::write(sidecar(&backup), serde_json::to_vec(&unbound).unwrap()).unwrap();
    fs::set_permissions(sidecar(&backup), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(transaction.join("transaction.json"), serde_json::to_vec(&json!({
        "version": 1, "phase": "backup-moved", "targetRoot": ".",
        "targets": [{"relative": ".collections/records", "collection": "records", "hadCurrent": true, "shouldInstall": true}],
        "ref": null
    })).unwrap()).unwrap();
    let before = snapshot(&fixture.root);
    assert_eq!(
        writer
            .recover_repository_materialization()
            .unwrap_err()
            .code(),
        NativeStorageErrorCode::CorruptMetadata
    );
    assert_eq!(snapshot(&fixture.root), before);
}

#[test]
fn profile_parsing_is_explicit_and_unknown_profiles_fail() {
    assert_eq!(
        FilesystemProfile::parse("native").unwrap(),
        FilesystemProfile::Native
    );
    assert_eq!(
        FilesystemProfile::parse("efs-sidecar-v1").unwrap(),
        FilesystemProfile::EfsSidecarV1
    );
    assert_eq!(FilesystemProfile::Native.as_str(), "native");
    assert_eq!(FilesystemProfile::EfsSidecarV1.as_str(), "efs-sidecar-v1");
    for unknown in ["", "efs", "EFS-SIDECAR-V1", "efs-sidecar-v2"] {
        assert!(FilesystemProfile::parse(unknown).is_err());
    }
}

#[test]
fn read_open_never_initializes_and_writer_requires_its_lease() {
    let fixture = Fixture::empty();
    let descriptor = fixture.root.join(".fylo-catalog/filesystem-profile.json");
    assert!(NativeRoot::open_profiled(fixture.options()).is_err());
    assert!(!descriptor.exists(), "a reader initialized the root");
    assert!(
        NativeWriteRoot::open_profiled(
            RootOpenOptions::new(&fixture.root).with_profile(FilesystemProfile::EfsSidecarV1)
        )
        .is_err()
    );
    assert!(
        !descriptor.exists(),
        "a writer without a lease initialized the root"
    );
    let _writer = fixture.writer();
    assert!(descriptor.is_file());
    let before = fs::read(&descriptor).unwrap();
    let _reader = fixture.reader();
    assert_eq!(fs::read(descriptor).unwrap(), before);
    assert!(
        NativeRoot::open(&fixture.root).is_err(),
        "native reader accepted EFS root"
    );
    assert!(
        NativeWriteRoot::open(&fixture.root).is_err(),
        "native writer accepted EFS root"
    );
}

#[test]
fn native_default_does_not_create_a_profile_descriptor() {
    let fixture = Fixture::empty();
    let _reader = NativeRoot::open_profiled(RootOpenOptions::new(&fixture.root)).unwrap();
    let _writer = NativeWriteRoot::open_profiled(RootOpenOptions::new(&fixture.root)).unwrap();
    assert!(
        !fixture
            .root
            .join(".fylo-catalog/filesystem-profile.json")
            .exists()
    );
}

#[test]
fn efs_initialization_refuses_a_nonempty_unconverted_root() {
    let fixture = Fixture::empty();
    fs::write(fixture.root.join("unconverted-record"), b"retained").unwrap();
    assert!(NativeWriteRoot::open_profiled(fixture.options()).is_err());
    assert_eq!(
        fs::read(fixture.root.join("unconverted-record")).unwrap(),
        b"retained"
    );
    assert!(
        !fixture
            .root
            .join(".fylo-catalog/filesystem-profile.json")
            .exists()
    );
}

#[test]
fn document_and_raw_metadata_are_mandatory_private_manifests() {
    let fixture = Fixture::empty();
    let _writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let document = records.read_document(ID).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&document.bytes).unwrap()["score"],
        42
    );
    assert_eq!(
        records.read_custom_metadata(ID).unwrap()["reviewer"],
        "before"
    );
    assert_eq!(document.access.unwrap().mode, 0o640);
    assert_private_sidecar(&document.path);
    let manifest = attributes(&document.path);
    assert!(manifest.contains_key("user.fylo.access"));
    assert!(manifest.contains_key("user.fylo.meta.reviewer"));

    let assets = fixture.reader().collection("assets").unwrap();
    let raw = assets.read_raw_file(RAW_ID).unwrap();
    assert_eq!(raw.key, "/seed.bin");
    assert_eq!(raw.bytes, [0, 1, 2, 255]);
    assert_eq!(raw.custom_metadata["source"], "seed");
    assert_eq!(raw.access_descriptor.unwrap().mode, 0o640);
    assert_eq!(assets.raw_file_ids().unwrap(), [RAW_ID]);
    assert_private_sidecar(&raw.path);
    assert!(attributes(&raw.path).contains_key("user.fylo.key"));
}

#[test]
fn metadata_replacement_and_removal_preserve_payload_time_and_access() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let actor = fixture.actor();
    let records = fixture.reader().collection("records").unwrap();
    let before = records.read_document(ID).unwrap();
    let before_bytes = fs::read(&before.path).unwrap();
    writer
        .set_record_metadata(
            "records",
            ID,
            json!({"reviewer": null, "nested": [1, true, "x"]})
                .as_object()
                .unwrap(),
            Some(&actor),
        )
        .unwrap();
    let metadata = records.read_custom_metadata(ID).unwrap();
    assert!(!metadata.contains_key("reviewer"));
    assert_eq!(metadata["nested"], json!([1, true, "x"]));
    let first_stamp = attributes(&before.path)["user.fylo.meta-updated-at"].clone();
    writer
        .replace_record_metadata("records", ID, json!({}).as_object().unwrap(), Some(&actor))
        .unwrap();
    assert!(records.read_custom_metadata(ID).unwrap().is_empty());
    assert!(
        sidecar(&before.path).is_file(),
        "attribute removal removed mandatory manifest"
    );
    let after = records.read_document(ID).unwrap();
    assert_eq!(after.bytes, before_bytes);
    assert_eq!(after.modified_millis, before.modified_millis);
    assert_eq!(after.access, before.access);
    let stamp = attributes(&before.path)["user.fylo.meta-updated-at"].clone();
    assert!(
        BASE64.decode(stamp).unwrap() > BASE64.decode(first_stamp).unwrap(),
        "metadata timestamp did not advance"
    );
    assert_private_sidecar(&before.path);
}

#[test]
fn delete_restore_sql_delete_and_reshard_move_entire_pairs() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let actor = fixture.actor();
    let records = fixture.reader().collection("records").unwrap();
    let before = records.read_document(ID).unwrap();
    let manifest = fs::read(sidecar(&before.path)).unwrap();
    writer.delete_document("records", ID, Some(&actor)).unwrap();
    assert!(!before.path.exists());
    assert!(!sidecar(&before.path).exists());
    let deleted = records.read_deleted_document(ID).unwrap();
    assert_eq!(fs::read(sidecar(&deleted.path)).unwrap(), manifest);
    assert_eq!(deleted.access, before.access);
    writer
        .restore_document("records", ID, Some(&actor))
        .unwrap();
    assert!(!deleted.path.exists());
    assert!(!sidecar(&deleted.path).exists());
    assert_eq!(fs::read(sidecar(&before.path)).unwrap(), manifest);
    writer
        .execute_sql_mutation(
            "DELETE FROM records WHERE name = 'Ada'",
            Some(&actor),
            WriteAccess::default(),
        )
        .unwrap();
    let deleted = records.read_deleted_document(ID).unwrap();
    assert_eq!(fs::read(sidecar(&deleted.path)).unwrap(), manifest);
    writer
        .restore_document("records", ID, Some(&actor))
        .unwrap();
    assert_eq!(writer.reshard_collection("records", 3).unwrap(), 1);
    let moved = fixture
        .reader()
        .collection("records")
        .unwrap()
        .read_document(ID)
        .unwrap();
    assert_ne!(moved.path, before.path);
    assert!(!before.path.exists());
    assert!(!sidecar(&before.path).exists());
    assert_eq!(fs::read(sidecar(&moved.path)).unwrap(), manifest);
    assert_eq!(moved.modified_millis, before.modified_millis);
    assert_eq!(moved.access, before.access);
    assert_eq!(
        fixture
            .reader()
            .collection("records")
            .unwrap()
            .document_ids()
            .unwrap(),
        [ID]
    );

    let assets = fixture.reader().collection("assets").unwrap();
    let raw_before = assets.read_raw_file(RAW_ID).unwrap();
    writer
        .delete_document("assets", RAW_ID, Some(&actor))
        .unwrap();
    let raw_deleted = assets.read_deleted_raw_file(RAW_ID).unwrap();
    assert_eq!(raw_deleted.key, raw_before.key);
    assert_eq!(raw_deleted.custom_metadata, raw_before.custom_metadata);
    writer
        .restore_document("assets", RAW_ID, Some(&actor))
        .unwrap();
    assert_eq!(writer.reshard_collection("assets", 3).unwrap(), 1);
    let raw_after = fixture
        .reader()
        .collection("assets")
        .unwrap()
        .read_raw_file(RAW_ID)
        .unwrap();
    assert_eq!(raw_after.key, raw_before.key);
    assert_eq!(raw_after.checksum_sha256, raw_before.checksum_sha256);
    assert_eq!(raw_after.access_descriptor, raw_before.access_descriptor);
    assert_eq!(raw_after.modified_millis, raw_before.modified_millis);
}

#[test]
fn missing_corrupt_duplicate_and_oversized_manifests_fail_closed() {
    let fixture = Fixture::empty();
    let _writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let payload = records.read_document(ID).unwrap().path;
    let manifest = sidecar(&payload);
    let original = fs::read(&manifest).unwrap();
    fs::remove_file(&manifest).unwrap();
    assert!(records.read_document(ID).is_err());
    assert!(records.read_custom_metadata(ID).is_err());
    let too_large = BASE64.encode(vec![b'x'; 60 * 1024 + 1]);
    let invalid_cases = [
        b"not-json".to_vec(),
        br#"{"user.fylo.access":"!invalid-base64!"}"#.to_vec(),
        br#"{"user.fylo.access":"e30=","user.fylo.access":"e30="}"#.to_vec(),
        serde_json::to_vec(&json!({"user.fylo.meta.large": too_large})).unwrap(),
        vec![b' '; 16 * 1024 * 1024 + 1],
        serde_json::to_vec(&json!({"user.fylo.meta.": BASE64.encode(b"true")})).unwrap(),
        serde_json::to_vec(&BTreeMap::from([(
            format!("user.fylo.meta.{}", "x".repeat(65)),
            BASE64.encode(b"true"),
        )]))
        .unwrap(),
        serde_json::to_vec(&json!({"user.fylo.meta.a\0b": BASE64.encode(b"true")})).unwrap(),
        serde_json::to_vec(&json!({"user.fylo.meta.a/b": BASE64.encode(b"true")})).unwrap(),
        serde_json::to_vec(&json!({"user.fylo.meta.a\\b": BASE64.encode(b"true")})).unwrap(),
    ];
    for invalid in invalid_cases {
        fs::write(&manifest, invalid).unwrap();
        assert!(records.read_document(ID).is_err());
        assert!(records.read_custom_metadata(ID).is_err());
    }
    fs::remove_file(&manifest).unwrap();
    let external = fixture.parent.join("external-manifest");
    fs::write(&external, &original).unwrap();
    symlink(&external, &manifest).unwrap();
    assert!(records.read_document(ID).is_err());
    assert!(records.read_custom_metadata(ID).is_err());
}

#[test]
fn orphan_destination_manifest_rejects_pair_move_without_losing_source() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let before = records.read_document(ID).unwrap();
    let original = fs::read(sidecar(&before.path)).unwrap();
    let relative = before
        .path
        .strip_prefix(records.path().join("docs"))
        .unwrap();
    let destination = records.path().join(".deleted").join(relative);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(sidecar(&destination), b"{}").unwrap();
    assert!(writer.delete_document("records", ID, None).is_err());
    assert_eq!(records.read_document(ID).unwrap().bytes, before.bytes);
    assert_eq!(fs::read(sidecar(&before.path)).unwrap(), original);
    assert_eq!(fs::read(sidecar(&destination)).unwrap(), b"{}");
    assert!(!destination.exists());
}

#[test]
fn cross_owner_projection_and_denied_actor_do_not_mutate_either_member() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let before = records.read_document(ID).unwrap();
    let manifest = fs::read(sidecar(&before.path)).unwrap();
    let native = fs::metadata(&before.path).unwrap();
    let foreign_uid = native.uid().wrapping_add(1).max(1);
    assert!(
        writer
            .set_record_access(
                "records",
                ID,
                WriteAccess {
                    uid: Some(foreign_uid),
                    ..WriteAccess::default()
                },
                None
            )
            .is_err()
    );
    let unrelated_group = native.gid().wrapping_add(1).max(1);
    assert!(
        writer
            .set_record_access(
                "records",
                ID,
                WriteAccess {
                    gid: Some(unrelated_group),
                    ..WriteAccess::default()
                },
                None
            )
            .is_err()
    );
    let anonymous = writer
        .set_record_metadata(
            "records",
            ID,
            json!({"reviewer": "anonymous"}).as_object().unwrap(),
            None,
        )
        .unwrap_err();
    assert_eq!(anonymous.code(), NativeStorageErrorCode::PermissionDenied);
    let denied = WriteActor::new(foreign_uid, []);
    let error = writer
        .set_record_metadata(
            "records",
            ID,
            json!({"reviewer": "denied"}).as_object().unwrap(),
            Some(&denied),
        )
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::PermissionDenied);
    assert_eq!(records.read_document(ID).unwrap().bytes, before.bytes);
    assert_eq!(fs::read(sidecar(&before.path)).unwrap(), manifest);
    assert_eq!(fs::metadata(&before.path).unwrap().uid(), native.uid());
    assert_eq!(fs::metadata(&before.path).unwrap().gid(), native.gid());
    assert_eq!(fs::metadata(&before.path).unwrap().mode(), native.mode());
    assert_eq!(
        records.read_document(ID).unwrap().modified_millis,
        before.modified_millis
    );
    assert!(!writer.recover_collection("records").unwrap());
}

#[test]
fn unsupported_owner_unreadable_modes_are_rejected_before_record_mutation() {
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32, u32, u32, SystemTime)> {
        fn visit(
            root: &Path,
            path: &Path,
            entries: &mut BTreeMap<PathBuf, (Vec<u8>, u32, u32, u32, SystemTime)>,
        ) {
            let metadata = fs::symlink_metadata(path).unwrap();
            entries.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (
                    if metadata.is_file() {
                        fs::read(path).unwrap()
                    } else {
                        Vec::new()
                    },
                    metadata.mode(),
                    metadata.uid(),
                    metadata.gid(),
                    metadata.modified().unwrap(),
                ),
            );
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), entries);
                }
            }
        }
        let mut entries = BTreeMap::new();
        visit(root, root, &mut entries);
        entries
    }

    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let before = snapshot(&fixture.root);
    let owner = fixture.parent.join(".root.fylo-root-owner.lock.json");
    let owner_before = fs::read(&owner).unwrap();
    for mode in [0o000, 0o200, 0o030] {
        let access = WriteAccess {
            mode: Some(mode),
            ..WriteAccess::default()
        };
        let error = writer
            .set_record_access("records", ID, access, None)
            .unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
        assert_eq!(snapshot(&fixture.root), before);
        let error = writer
            .put_document(
                "records",
                RAW_ID,
                br#"{"name":"refused"}"#,
                PutDocumentOptions {
                    access,
                    ..PutDocumentOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
        assert_eq!(snapshot(&fixture.root), before);
        let error = writer
            .put_raw_file(
                "assets",
                "4VRNF52JPCR",
                b"refused",
                &PutRawFileOptions {
                    key: "/refused.bin".into(),
                    extension: ".bin".into(),
                    access,
                    ..PutRawFileOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
        assert_eq!(snapshot(&fixture.root), before);
        assert_eq!(fs::read(&owner).unwrap(), owner_before);
    }
}

#[test]
fn a_lost_root_owner_cannot_continue_writing_or_recovering() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let before = records.read_document(ID).unwrap();
    let manifest = fs::read(sidecar(&before.path)).unwrap();
    let owner_metadata = fixture.parent.join(".root.fylo-root-owner.lock.json");
    let mut owner: Value = serde_json::from_slice(&fs::read(&owner_metadata).unwrap()).unwrap();
    owner["owner"] = json!("another-owner-generation");
    fs::write(owner_metadata, serde_json::to_vec(&owner).unwrap()).unwrap();
    assert_eq!(
        fixture.lease.assert_owned().unwrap_err().code(),
        NativeStorageErrorCode::RootLeaseLost
    );
    assert_eq!(
        writer
            .set_record_metadata(
                "records",
                ID,
                json!({"reviewer": "lost"}).as_object().unwrap(),
                None
            )
            .unwrap_err()
            .code(),
        NativeStorageErrorCode::RootLeaseLost
    );
    assert_eq!(
        writer.recover_collection("records").unwrap_err().code(),
        NativeStorageErrorCode::RootLeaseLost
    );
    assert_eq!(fs::read(&before.path).unwrap(), before.bytes);
    assert_eq!(fs::read(sidecar(&before.path)).unwrap(), manifest);
}

#[test]
fn profiled_repository_materialization_and_restore_keep_metadata_history() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let actor = fixture.actor();
    let before = fixture
        .reader()
        .collection("records")
        .unwrap()
        .read_document(ID)
        .unwrap();
    let attrs = attributes(&before.path);
    let first = writer
        .commit_if_dirty("profiled baseline")
        .unwrap()
        .unwrap();
    let branch = writer.checkout_repository("review", true).unwrap();
    let branch_root = PathBuf::from(branch["root"].as_str().unwrap());
    let options = || {
        RootOpenOptions::new(&branch_root)
            .with_repository(&fixture.root)
            .with_profile(FilesystemProfile::EfsSidecarV1)
            .with_lease(fixture.lease.clone())
    };
    let branch_reader = NativeRoot::open_profiled(options()).unwrap();
    let records = branch_reader.collection("records").unwrap();
    let copied = records.read_document(ID).unwrap();
    assert_eq!(copied.bytes, before.bytes);
    assert_eq!(copied.modified_millis, before.modified_millis);
    assert_eq!(copied.access, before.access);
    assert_eq!(attributes(&copied.path), attrs);
    assert_private_sidecar(&copied.path);
    let branch_writer = NativeWriteRoot::open_profiled(options()).unwrap();
    branch_writer
        .set_record_metadata(
            "records",
            ID,
            json!({"reviewer": "branch"}).as_object().unwrap(),
            Some(&actor),
        )
        .unwrap();
    let second = branch_writer
        .commit_if_dirty("profiled metadata change")
        .unwrap()
        .unwrap();
    assert_ne!(second, first);
    branch_writer
        .restore_repository_commit(&first, true)
        .unwrap();
    let restored = records.read_document(ID).unwrap();
    assert_eq!(restored.bytes, before.bytes);
    // Existing version-2 objects archive attributes, not historical payload
    // mtime; restoration retains Native materialization-time semantics.
    assert_eq!(restored.access, before.access);
    assert_eq!(attributes(&restored.path), attrs);
    assert_eq!(
        records.read_custom_metadata(ID).unwrap()["reviewer"],
        "before"
    );
    assert_eq!(records.document_ids().unwrap(), [ID]);
    assert_private_sidecar(&restored.path);
}

#[test]
fn an_open_root_refuses_removed_or_changed_profile_descriptors() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let before = records.read_document(ID).unwrap();
    let sidecar_before = fs::read(sidecar(&before.path)).unwrap();
    let descriptor = fixture.root.join(".fylo-catalog/filesystem-profile.json");
    let original = fs::read(&descriptor).unwrap();
    let mut mismatched: Value = serde_json::from_slice(&original).unwrap();
    mismatched["uid"] = json!(fs::metadata(&fixture.root).unwrap().uid().wrapping_add(1));
    let mut unknown: Value = serde_json::from_slice(&original).unwrap();
    unknown["unrecognized"] = json!(true);
    for invalid in [
        b"{}".to_vec(),
        serde_json::to_vec(&mismatched).unwrap(),
        serde_json::to_vec(&unknown).unwrap(),
    ] {
        fs::write(&descriptor, invalid).unwrap();
        assert!(records.read_document(ID).is_err());
        assert!(
            writer
                .set_record_metadata(
                    "records",
                    ID,
                    json!({"reviewer": "must refuse"}).as_object().unwrap(),
                    None
                )
                .is_err()
        );
        assert!(writer.recover_collection("records").is_err());
        assert_eq!(fs::read(&before.path).unwrap(), before.bytes);
        assert_eq!(fs::read(sidecar(&before.path)).unwrap(), sidecar_before);
        fs::write(&descriptor, &original).unwrap();
    }
    fs::remove_file(&descriptor).unwrap();
    assert!(records.read_document(ID).is_err());
    assert!(writer.recover_collection("records").is_err());
    assert_eq!(fs::read(&before.path).unwrap(), before.bytes);
    assert_eq!(fs::read(sidecar(&before.path)).unwrap(), sidecar_before);
}

#[test]
fn a_descriptor_changed_after_admission_is_refused_even_when_recently_verified() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let marker = fixture.root.join(".fylo-catalog/filesystem-profile.json");
    let original = fs::read(&marker).unwrap();
    // Same length and owner, different bytes: only a fresh read can tell.
    let mut descriptor: Value = serde_json::from_slice(&original).unwrap();
    descriptor["profile"] = json!("efs-sidecar-v9");
    let changed = serde_json::to_vec(&descriptor).unwrap();
    assert_eq!(changed.len(), original.len());
    fs::write(&marker, &changed).unwrap();
    let refused = writer.put_document(
        "records",
        "4VRNF52JPCQ",
        br#"{"name":"Grace"}"#,
        PutDocumentOptions::default(),
    );
    assert_eq!(
        refused.err().unwrap().code(),
        NativeStorageErrorCode::CorruptMetadata
    );
    assert!(
        !fixture
            .root
            .join(".collections/records/docs")
            .read_dir()
            .unwrap()
            .any(|shard| { shard.unwrap().path().join("4VRNF52JPCQ.json").exists() })
    );
}
