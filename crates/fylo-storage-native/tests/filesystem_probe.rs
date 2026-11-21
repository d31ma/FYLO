//! The file-system probe of an app folder (ADR 0033 section 6): it chooses
//! extended attributes or sidecar files once, at creation, and checks the
//! recorded way at every writing open.
#![cfg(not(target_family = "wasm"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_storage_native::{
    FilesystemProbe, FilesystemProbeRecord, FilesystemProfile, MetadataStorage,
    NativeStorageErrorCode,
};

const NO_ATTRIBUTE_DIRECTORY: &str = "FYLO_TEST_NO_XATTR_DIR";
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);

impl Directory {
    fn new(parent: &Path) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = parent.join(format!(
            "fylo-filesystem-probe-test-{}-{stamp}-{}",
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
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn record(metadata: MetadataStorage, hard_links: bool) -> FilesystemProbeRecord {
    FilesystemProbeRecord::new(metadata, hard_links)
}

#[test]
fn a_local_disk_keeps_extended_attributes_and_hard_links() {
    let directory = Directory::temporary();

    let probed = FilesystemProbe::run(&directory.0, None).unwrap();

    assert_eq!(probed, record(MetadataStorage::ExtendedAttributes, true));
}

#[test]
fn the_probe_leaves_no_file_behind() {
    let directory = Directory::temporary();

    FilesystemProbe::run(&directory.0, None).unwrap();
    FilesystemProbe::verify(
        &directory.0,
        record(MetadataStorage::ExtendedAttributes, true),
    )
    .unwrap();

    assert_eq!(directory.names(), Vec::<String>::new());
}

#[test]
fn the_sidecar_override_chooses_sidecar_files_on_a_local_disk() {
    let directory = Directory::temporary();

    let probed = FilesystemProbe::run(&directory.0, Some(FilesystemProfile::EfsSidecarV1)).unwrap();

    assert_eq!(probed.metadata(), MetadataStorage::SidecarFiles);
    assert_eq!(probed.profile(), FilesystemProfile::EfsSidecarV1);
}

#[test]
fn the_native_override_keeps_extended_attributes_on_a_local_disk() {
    let directory = Directory::temporary();

    let probed = FilesystemProbe::run(&directory.0, Some(FilesystemProfile::Native)).unwrap();

    assert_eq!(probed.metadata(), MetadataStorage::ExtendedAttributes);
    assert_eq!(probed.profile(), FilesystemProfile::Native);
}

#[test]
fn a_missing_folder_fails_the_probe_with_efsprobe() {
    let directory = Directory::temporary();
    let missing = directory.0.join("missing");

    let error = FilesystemProbe::run(&missing, None).unwrap_err();

    assert_eq!(error.code(), NativeStorageErrorCode::FilesystemProbe);
    assert_eq!(error.code().as_str(), "EFSPROBE");
    assert!(error.message().contains("exclusive create"), "{error}");
    assert!(
        error.message().contains(&missing.display().to_string()),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn a_read_only_folder_fails_the_probe_with_efsprobe() {
    use std::os::unix::fs::PermissionsExt;

    let directory = Directory::temporary();
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o555)).unwrap();

    let error = FilesystemProbe::run(&directory.0, None).unwrap_err();
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(error.code(), NativeStorageErrorCode::FilesystemProbe);
}

#[test]
fn the_recorded_way_passes_its_open_check() {
    let directory = Directory::temporary();
    let probed = FilesystemProbe::run(&directory.0, None).unwrap();

    FilesystemProbe::verify(&directory.0, probed).unwrap();
}

#[test]
fn a_sidecar_record_without_hard_links_passes_its_open_check() {
    let directory = Directory::temporary();

    FilesystemProbe::verify(&directory.0, record(MetadataStorage::SidecarFiles, false)).unwrap();
}

#[test]
fn the_open_check_fails_with_efsprobe_when_the_folder_is_gone() {
    let directory = Directory::temporary();
    let missing = directory.0.join("missing");

    let error =
        FilesystemProbe::verify(&missing, record(MetadataStorage::ExtendedAttributes, true))
            .unwrap_err();

    assert_eq!(error.code(), NativeStorageErrorCode::FilesystemProbe);
    assert!(
        error.message().contains("recorded in format.json"),
        "{error}"
    );
}

#[test]
fn no_override_always_matches_the_record() {
    let recorded = record(MetadataStorage::SidecarFiles, false);

    FilesystemProbe::check_override(recorded, None).unwrap();
}

#[test]
fn the_same_override_matches_the_record() {
    let recorded = record(MetadataStorage::SidecarFiles, false);

    FilesystemProbe::check_override(recorded, Some(FilesystemProfile::EfsSidecarV1)).unwrap();
}

#[test]
fn another_override_is_refused_on_an_existing_app_folder() {
    let recorded = record(MetadataStorage::ExtendedAttributes, true);

    let error = FilesystemProbe::check_override(recorded, Some(FilesystemProfile::EfsSidecarV1))
        .unwrap_err();

    assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
    assert!(error.message().contains("efs-sidecar-v1"), "{error}");
    assert!(error.message().contains("xattr"), "{error}");
}

#[test]
fn the_record_is_the_probe_object_of_format_json() {
    let recorded = record(MetadataStorage::SidecarFiles, false);

    let json = serde_json::to_value(recorded).unwrap();

    assert_eq!(
        json,
        serde_json::json!({"metadata": "sidecar", "hardLinks": false})
    );
}

#[test]
fn the_record_reads_back_from_format_json() {
    let json = serde_json::json!({"metadata": "xattr", "hardLinks": true});

    let recorded: FilesystemProbeRecord = serde_json::from_value(json).unwrap();

    assert_eq!(recorded, record(MetadataStorage::ExtendedAttributes, true));
}

#[test]
fn a_record_with_an_unknown_field_is_refused() {
    let json = serde_json::json!({"metadata": "xattr", "hardLinks": true, "extra": 1});

    let result = serde_json::from_value::<FilesystemProbeRecord>(json);

    assert!(result.is_err());
}

#[test]
fn a_record_with_an_unknown_metadata_storage_is_refused() {
    let json = serde_json::json!({"metadata": "ads", "hardLinks": true});

    let result = serde_json::from_value::<FilesystemProbeRecord>(json);

    assert!(result.is_err());
}

#[test]
#[ignore = "needs FYLO_TEST_NO_XATTR_DIR on a file system without extended attributes"]
fn a_file_system_without_attributes_gets_sidecar_files() {
    let directory = Directory::without_attributes();

    let probed = FilesystemProbe::run(&directory.0, None).unwrap();

    assert_eq!(probed.metadata(), MetadataStorage::SidecarFiles);
    assert_eq!(directory.names(), Vec::<String>::new());
}

#[test]
#[ignore = "needs FYLO_TEST_NO_XATTR_DIR on a file system without extended attributes"]
fn the_native_override_fails_with_efsprobe_without_attributes() {
    let directory = Directory::without_attributes();

    let error = FilesystemProbe::run(&directory.0, Some(FilesystemProfile::Native)).unwrap_err();

    assert_eq!(error.code(), NativeStorageErrorCode::FilesystemProbe);
    assert!(error.message().contains("extended attributes"), "{error}");
}

#[test]
#[ignore = "needs FYLO_TEST_NO_XATTR_DIR on a file system without extended attributes"]
fn an_app_folder_copied_to_a_share_without_attributes_fails_its_open_check() {
    let directory = Directory::without_attributes();

    let error = FilesystemProbe::verify(
        &directory.0,
        record(MetadataStorage::ExtendedAttributes, true),
    )
    .unwrap_err();

    assert_eq!(error.code(), NativeStorageErrorCode::FilesystemProbe);
    assert!(
        error
            .message()
            .contains("extended attributes, recorded in format.json"),
        "{error}"
    );
    assert_eq!(directory.names(), Vec::<String>::new());
}
