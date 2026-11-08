//! A native writer checks that its root's filesystem keeps extended
//! attributes before it creates a bucket, and names the call and the file
//! when an attribute write fails there (#106). Readers, document
//! collections and the efs-sidecar-v1 profile never check.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_storage_native::{
    CollectionKind, FilesystemProfile, NativeRoot, NativeStorageErrorCode, NativeWriteRoot,
    PutDocumentOptions, RootLease, RootOpenOptions,
};
use serde_json::json;

const IDENTIFIER: &str = "4VRNF52JPCO";
const NO_ATTRIBUTE_DIRECTORY: &str = "FYLO_TEST_NO_XATTR_DIR";
const PROBE_PREFIX: &str = ".fylo-xattr-probe-";
const UNSUPPORTED: &str = "filesystem does not support extended attributes";
const REMEDY: &str = "use a local filesystem with extended attributes, the efs-sidecar-v1 filesystem profile, or a .vdbx database";
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);

impl Directory {
    fn new(parent: &Path) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = parent.join(format!(
            "fylo-xattr-probe-test-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn temporary() -> Self {
        Self::new(&std::env::temp_dir())
    }

    fn without_attributes() -> Self {
        Self::new(&PathBuf::from(
            std::env::var(NO_ATTRIBUTE_DIRECTORY).unwrap(),
        ))
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn probe_files(&self) -> Vec<String> {
        self.names()
            .into_iter()
            .filter(|name| name.starts_with(PROBE_PREFIX))
            .collect()
    }

    fn top_level_read_only(&self) {
        fs::set_permissions(&self.0, fs::Permissions::from_mode(0o555)).unwrap();
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn seed(writer: &NativeWriteRoot) {
    writer
        .create_collection("notes", CollectionKind::Document, None)
        .unwrap();
    writer
        .create_collection("assets", CollectionKind::File, None)
        .unwrap();
}

#[test]
fn creating_a_bucket_leaves_no_probe_file_behind() {
    let root = Directory::temporary();
    let writer = NativeWriteRoot::open(&root.0).unwrap();

    seed(&writer);

    assert_eq!(root.probe_files(), Vec::<String>::new());
}

#[test]
fn creating_a_bucket_probes_the_top_of_the_root() {
    let root = Directory::temporary();
    let writer = NativeWriteRoot::open(&root.0).unwrap();
    seed(&writer);
    root.top_level_read_only();

    let error = writer
        .create_collection("assets", CollectionKind::File, None)
        .unwrap_err();

    assert_eq!(error.code(), NativeStorageErrorCode::Io);
    assert!(error.to_string().contains("Permission denied"), "{error}");
}

#[test]
fn creating_a_document_collection_never_probes() {
    let root = Directory::temporary();
    let writer = NativeWriteRoot::open(&root.0).unwrap();
    seed(&writer);
    root.top_level_read_only();

    let created = writer
        .create_collection("drafts", CollectionKind::Document, None)
        .unwrap();

    assert!(created);
}

#[test]
fn an_efs_sidecar_writer_never_probes() {
    let root = Directory::temporary();
    let lease = RootLease::acquire_with_profile(&root.0, FilesystemProfile::EfsSidecarV1).unwrap();
    let writer = NativeWriteRoot::open_profiled(
        RootOpenOptions::new(&root.0)
            .with_profile(FilesystemProfile::EfsSidecarV1)
            .with_lease(lease),
    )
    .unwrap();
    seed(&writer);
    root.top_level_read_only();

    let created = writer
        .create_collection("assets", CollectionKind::File, None)
        .unwrap();

    assert!(!created);
}

#[test]
fn a_read_only_open_never_probes() {
    let root = Directory::temporary();
    seed(&NativeWriteRoot::open(&root.0).unwrap());
    root.top_level_read_only();

    let reader = NativeRoot::open(&root.0).unwrap();

    assert_eq!(reader.collection("assets").unwrap().name(), "assets");
}

#[test]
#[ignore = "needs FYLO_TEST_NO_XATTR_DIR on a filesystem without extended attributes"]
fn a_bucket_is_refused_on_a_filesystem_without_attributes() {
    let root = Directory::without_attributes();
    let writer = NativeWriteRoot::open(&root.0).unwrap();

    let error = writer
        .create_collection("assets", CollectionKind::File, None)
        .unwrap_err();

    assert_eq!(error.code(), NativeStorageErrorCode::Io);
    let message = error.to_string();
    assert!(
        message.contains("setxattr user.fylo.probe on "),
        "{message}"
    );
    assert!(message.contains(&root.0.display().to_string()), "{message}");
    assert!(message.contains(UNSUPPORTED), "{message}");
    assert!(message.contains(REMEDY), "{message}");
    assert_eq!(root.names(), Vec::<String>::new());
}

#[test]
#[ignore = "needs FYLO_TEST_NO_XATTR_DIR on a filesystem without extended attributes"]
fn documents_work_on_a_filesystem_without_attributes() {
    let root = Directory::without_attributes();
    let writer = NativeWriteRoot::open(&root.0).unwrap();
    writer
        .create_collection("notes", CollectionKind::Document, None)
        .unwrap();

    writer
        .put_document(
            "notes",
            IDENTIFIER,
            br#"{"title":"hello"}"#,
            PutDocumentOptions::default(),
        )
        .unwrap();

    let reader = NativeRoot::open(&root.0).unwrap();
    assert_eq!(reader.collection("notes").unwrap().name(), "notes");
}

#[test]
#[ignore = "needs FYLO_TEST_NO_XATTR_DIR on a filesystem without extended attributes"]
fn document_metadata_without_attributes_names_the_call_and_the_file() {
    let root = Directory::without_attributes();
    let writer = NativeWriteRoot::open(&root.0).unwrap();
    writer
        .create_collection("notes", CollectionKind::Document, None)
        .unwrap();

    let error = writer
        .put_document(
            "notes",
            IDENTIFIER,
            br#"{"title":"hello"}"#,
            PutDocumentOptions {
                metadata: BTreeMap::from([("source".to_owned(), json!("test"))]),
            },
        )
        .unwrap_err();

    assert_eq!(error.code(), NativeStorageErrorCode::Io);
    let message = error.to_string();
    assert!(
        message.contains("setxattr user.fylo.meta.source on "),
        "{message}"
    );
    assert!(message.contains(IDENTIFIER), "{message}");
    assert!(message.contains(UNSUPPORTED), "{message}");
    assert!(message.contains(REMEDY), "{message}");
}
