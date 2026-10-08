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
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let parent = std::env::temp_dir().join(format!(
            "fylo-efs-profile-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let root = parent.join("root");
        fs::create_dir_all(&root).unwrap();
        let lease = RootLease::acquire_with_profile(&root, FilesystemProfile::EfsSidecarV1)
            .unwrap();
        assert!(lease.kernel_enforced(), "profile needs an enforced kernel lease");
        Self { parent, root, lease }
    }

    fn options(&self) -> RootOpenOptions {
        RootOpenOptions::new(&self.root)
            .with_profile(FilesystemProfile::EfsSidecarV1)
            .with_lease(self.lease.clone())
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(self.options()).unwrap()
    }

    fn reader(&self) -> NativeRoot {
        NativeRoot::open_profiled(self.options()).unwrap()
    }

    fn seed(&self) -> NativeWriteRoot {
        let writer = self.writer();
        writer.create_collection("records", CollectionKind::Document, None).unwrap();
        writer.create_collection("assets", CollectionKind::File, None).unwrap();
        writer.put_document(
            "records", ID, br#"{"name":"Ada","score":42}"#,
            PutDocumentOptions {
                metadata: BTreeMap::from([
                    ("reviewer".into(), json!("before")),
                    ("typed".into(), json!({"accepted": true, "count": 2})),
                ]),
                access: WriteAccess { mode: Some(0o640), ..WriteAccess::default() },
            },
        ).unwrap();
        writer.put_raw_file(
            "assets", RAW_ID, &[0, 1, 2, 255],
            &PutRawFileOptions {
                key: "/seed.bin".into(),
                extension: ".bin".into(),
                metadata: BTreeMap::from([("source".into(), json!("seed"))]),
                access: WriteAccess { mode: Some(0o640), ..WriteAccess::default() },
            },
        ).unwrap();
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

fn assert_private_sidecar(payload: &Path) {
    let manifest = fs::symlink_metadata(sidecar(payload)).unwrap();
    assert!(manifest.file_type().is_file());
    assert_eq!(manifest.permissions().mode() & 0o777, 0o600);
    assert_eq!(manifest.uid(), fs::metadata(payload).unwrap().uid());
    assert_eq!(manifest.gid(), fs::metadata(payload).unwrap().gid());
}

#[test]
fn profile_parsing_is_explicit_and_unknown_profiles_fail() {
    assert_eq!(FilesystemProfile::parse("native").unwrap(), FilesystemProfile::Native);
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
            RootOpenOptions::new(&fixture.root)
                .with_profile(FilesystemProfile::EfsSidecarV1)
        ).is_err()
    );
    assert!(!descriptor.exists(), "a writer without a lease initialized the root");
    let _writer = fixture.writer();
    assert!(descriptor.is_file());
    let before = fs::read(&descriptor).unwrap();
    let _reader = fixture.reader();
    assert_eq!(fs::read(descriptor).unwrap(), before);
    assert!(NativeRoot::open(&fixture.root).is_err(), "native reader accepted EFS root");
    assert!(NativeWriteRoot::open(&fixture.root).is_err(), "native writer accepted EFS root");
}

#[test]
fn native_default_does_not_create_a_profile_descriptor() {
    let fixture = Fixture::empty();
    let _reader = NativeRoot::open_profiled(RootOpenOptions::new(&fixture.root)).unwrap();
    let _writer = NativeWriteRoot::open_profiled(RootOpenOptions::new(&fixture.root)).unwrap();
    assert!(!fixture.root.join(".fylo-catalog/filesystem-profile.json").exists());
}

#[test]
fn efs_initialization_refuses_a_nonempty_unconverted_root() {
    let fixture = Fixture::empty();
    fs::write(fixture.root.join("unconverted-record"), b"retained").unwrap();
    assert!(NativeWriteRoot::open_profiled(fixture.options()).is_err());
    assert_eq!(fs::read(fixture.root.join("unconverted-record")).unwrap(), b"retained");
    assert!(!fixture.root.join(".fylo-catalog/filesystem-profile.json").exists());
}

#[test]
fn document_and_raw_metadata_are_mandatory_private_manifests() {
    let fixture = Fixture::empty();
    let _writer = fixture.seed();
    let records = fixture.reader().collection("records").unwrap();
    let document = records.read_document(ID).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&document.bytes).unwrap()["score"], 42);
    assert_eq!(records.read_custom_metadata(ID).unwrap()["reviewer"], "before");
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
    let records = fixture.reader().collection("records").unwrap();
    let before = records.read_document(ID).unwrap();
    let before_bytes = fs::read(&before.path).unwrap();
    writer.set_record_metadata(
        "records", ID, json!({"reviewer": null, "nested": [1, true, "x"]}).as_object().unwrap(), None
    ).unwrap();
    let metadata = records.read_custom_metadata(ID).unwrap();
    assert!(!metadata.contains_key("reviewer"));
    assert_eq!(metadata["nested"], json!([1, true, "x"]));
    let first_stamp = attributes(&before.path)["user.fylo.meta-updated-at"].clone();
    writer.replace_record_metadata("records", ID, json!({}).as_object().unwrap(), None).unwrap();
    assert!(records.read_custom_metadata(ID).unwrap().is_empty());
    assert!(sidecar(&before.path).is_file(), "attribute removal removed mandatory manifest");
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
    let records = fixture.reader().collection("records").unwrap();
    let before = records.read_document(ID).unwrap();
    let manifest = fs::read(sidecar(&before.path)).unwrap();
    writer.delete_document("records", ID, None).unwrap();
    assert!(!before.path.exists());
    assert!(!sidecar(&before.path).exists());
    let deleted = records.read_deleted_document(ID).unwrap();
    assert_eq!(fs::read(sidecar(&deleted.path)).unwrap(), manifest);
    assert_eq!(deleted.access, before.access);
    writer.restore_document("records", ID, None).unwrap();
    assert!(!deleted.path.exists());
    assert!(!sidecar(&deleted.path).exists());
    assert_eq!(fs::read(sidecar(&before.path)).unwrap(), manifest);
    writer.execute_sql_mutation(
        "DELETE FROM records WHERE name = 'Ada'", None, WriteAccess::default()
    ).unwrap();
    let deleted = records.read_deleted_document(ID).unwrap();
    assert_eq!(fs::read(sidecar(&deleted.path)).unwrap(), manifest);
    writer.restore_document("records", ID, None).unwrap();
    assert_eq!(writer.reshard_collection("records", 3).unwrap(), 1);
    let moved = fixture.reader().collection("records").unwrap().read_document(ID).unwrap();
    assert_ne!(moved.path, before.path);
    assert!(!before.path.exists());
    assert!(!sidecar(&before.path).exists());
    assert_eq!(fs::read(sidecar(&moved.path)).unwrap(), manifest);
    assert_eq!(moved.modified_millis, before.modified_millis);
    assert_eq!(moved.access, before.access);
    assert_eq!(fixture.reader().collection("records").unwrap().document_ids().unwrap(), [ID]);

    let assets = fixture.reader().collection("assets").unwrap();
    let raw_before = assets.read_raw_file(RAW_ID).unwrap();
    writer.delete_document("assets", RAW_ID, None).unwrap();
    let raw_deleted = assets.read_deleted_raw_file(RAW_ID).unwrap();
    assert_eq!(raw_deleted.key, raw_before.key);
    assert_eq!(raw_deleted.custom_metadata, raw_before.custom_metadata);
    writer.restore_document("assets", RAW_ID, None).unwrap();
    assert_eq!(writer.reshard_collection("assets", 3).unwrap(), 1);
    let raw_after = fixture.reader().collection("assets").unwrap().read_raw_file(RAW_ID).unwrap();
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
            format!("user.fylo.meta.{}", "x".repeat(65)), BASE64.encode(b"true"),
        )])).unwrap(),
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
    let relative = before.path.strip_prefix(records.path().join("docs")).unwrap();
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
    assert!(writer.set_record_access(
        "records", ID, WriteAccess { uid: Some(foreign_uid), ..WriteAccess::default() }, None
    ).is_err());
    let foreign_gid = native.gid().wrapping_add(1).max(1);
    assert!(writer.set_record_access(
        "records", ID, WriteAccess { gid: Some(foreign_gid), ..WriteAccess::default() }, None
    ).is_err());
    let denied = WriteActor::new(foreign_uid, []);
    let error = writer.set_record_metadata(
        "records", ID, json!({"reviewer": "denied"}).as_object().unwrap(), Some(&denied)
    ).unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::PermissionDenied);
    assert_eq!(records.read_document(ID).unwrap().bytes, before.bytes);
    assert_eq!(fs::read(sidecar(&before.path)).unwrap(), manifest);
    assert_eq!(fs::metadata(&before.path).unwrap().uid(), native.uid());
    assert_eq!(fs::metadata(&before.path).unwrap().gid(), native.gid());
    assert_eq!(fs::metadata(&before.path).unwrap().mode(), native.mode());
    assert_eq!(records.read_document(ID).unwrap().modified_millis, before.modified_millis);
    assert!(!writer.recover_collection("records").unwrap());
}

#[test]
fn unsupported_owner_unreadable_modes_are_rejected_before_record_mutation() {
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32, u32, u32, SystemTime)> {
        fn visit(
            root: &Path, path: &Path,
            entries: &mut BTreeMap<PathBuf, (Vec<u8>, u32, u32, u32, SystemTime)>,
        ) {
            let metadata = fs::symlink_metadata(path).unwrap();
            entries.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (
                    if metadata.is_file() { fs::read(path).unwrap() } else { Vec::new() },
                    metadata.mode(), metadata.uid(), metadata.gid(),
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
        let access = WriteAccess { mode: Some(mode), ..WriteAccess::default() };
        let error = writer.set_record_access("records", ID, access, None).unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
        assert_eq!(snapshot(&fixture.root), before);
        let error = writer.put_document(
            "records", RAW_ID, br#"{"name":"refused"}"#,
            PutDocumentOptions { access, ..PutDocumentOptions::default() },
        ).unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
        assert_eq!(snapshot(&fixture.root), before);
        let error = writer.put_raw_file(
            "assets", "4VRNF52JPCR", b"refused",
            &PutRawFileOptions {
                key: "/refused.bin".into(), extension: ".bin".into(), access,
                ..PutRawFileOptions::default()
            },
        ).unwrap_err();
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
    assert_eq!(fixture.lease.assert_owned().unwrap_err().code(), NativeStorageErrorCode::RootLeaseLost);
    assert_eq!(writer.set_record_metadata(
        "records", ID, json!({"reviewer": "lost"}).as_object().unwrap(), None
    ).unwrap_err().code(), NativeStorageErrorCode::RootLeaseLost);
    assert_eq!(writer.recover_collection("records").unwrap_err().code(), NativeStorageErrorCode::RootLeaseLost);
    assert_eq!(fs::read(&before.path).unwrap(), before.bytes);
    assert_eq!(fs::read(sidecar(&before.path)).unwrap(), manifest);
}


#[test]
fn profiled_repository_materialization_and_restore_keep_metadata_history() {
    let fixture = Fixture::empty();
    let writer = fixture.seed();
    let before = fixture.reader().collection("records").unwrap().read_document(ID).unwrap();
    let attrs = attributes(&before.path);
    let first = writer.commit_if_dirty("profiled baseline").unwrap().unwrap();
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
    branch_writer.set_record_metadata(
        "records", ID, json!({"reviewer": "branch"}).as_object().unwrap(), None
    ).unwrap();
    let second = branch_writer.commit_if_dirty("profiled metadata change").unwrap().unwrap();
    assert_ne!(second, first);
    branch_writer.restore_repository_commit(&first, true).unwrap();
    let restored = records.read_document(ID).unwrap();
    assert_eq!(restored.bytes, before.bytes);
    // Existing version-2 objects archive attributes, not historical payload
    // mtime; restoration retains Native materialization-time semantics.
    assert_eq!(restored.access, before.access);
    assert_eq!(attributes(&restored.path), attrs);
    assert_eq!(records.read_custom_metadata(ID).unwrap()["reviewer"], "before");
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
    for invalid in [b"{}".to_vec(), serde_json::to_vec(&mismatched).unwrap(),
        serde_json::to_vec(&unknown).unwrap()]
    {
        fs::write(&descriptor, invalid).unwrap();
        assert!(records.read_document(ID).is_err());
        assert!(writer.set_record_metadata(
            "records", ID, json!({"reviewer": "must refuse"}).as_object().unwrap(), None
        ).is_err());
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
