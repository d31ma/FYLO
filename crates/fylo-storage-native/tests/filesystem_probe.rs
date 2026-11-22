//! The file-system probe of an app folder (ADR 0033 section 6): it chooses
//! extended attributes or sidecar files once, at creation, and checks the
//! recorded way at every writing open.
#![cfg(not(target_family = "wasm"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fylo_storage_native::layout::{
    AppFolder, FormatMarker, MetadataStore, ProbeRecord, RootLayout,
};
use fylo_storage_native::{FilesystemProbe, FilesystemProfile, NativeStorageErrorCode};

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
        names_in(&self.0)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn record(metadata: MetadataStore, hard_links: bool) -> ProbeRecord {
    ProbeRecord::new(metadata, hard_links)
}

#[test]
fn a_local_disk_keeps_extended_attributes_and_hard_links() {
    let directory = Directory::temporary();

    let probed = FilesystemProbe::run(&directory.0, None).unwrap();

    assert_eq!(probed.metadata(), MetadataStore::ExtendedAttributes);
    assert!(probed.hard_links());
}

#[test]
fn a_local_disk_records_the_largest_attribute_value_it_holds() {
    let directory = Directory::temporary();

    let probed = FilesystemProbe::run(&directory.0, None).unwrap();

    assert!(probed.max_attribute_bytes().unwrap() >= 1024, "{probed:?}");
}

#[test]
fn sidecar_storage_records_no_attribute_limit() {
    let directory = Directory::temporary();

    let probed = FilesystemProbe::run(&directory.0, Some(FilesystemProfile::EfsSidecarV1)).unwrap();

    assert_eq!(probed.max_attribute_bytes(), None);
}

#[test]
fn the_probe_leaves_no_file_behind() {
    let directory = Directory::temporary();

    FilesystemProbe::run(&directory.0, None).unwrap();
    FilesystemProbe::verify(
        &directory.0,
        record(MetadataStore::ExtendedAttributes, true),
    )
    .unwrap();

    assert_eq!(directory.names(), Vec::<String>::new());
}

#[test]
fn the_sidecar_override_chooses_sidecar_files_on_a_local_disk() {
    let directory = Directory::temporary();

    let probed = FilesystemProbe::run(&directory.0, Some(FilesystemProfile::EfsSidecarV1)).unwrap();

    assert_eq!(probed.metadata(), MetadataStore::SidecarFiles);
    assert_eq!(
        FilesystemProbe::profile_of(probed.metadata()),
        FilesystemProfile::EfsSidecarV1
    );
}

#[test]
fn the_native_override_keeps_extended_attributes_on_a_local_disk() {
    let directory = Directory::temporary();

    let probed = FilesystemProbe::run(&directory.0, Some(FilesystemProfile::Native)).unwrap();

    assert_eq!(probed.metadata(), MetadataStore::ExtendedAttributes);
    assert_eq!(
        FilesystemProbe::profile_of(probed.metadata()),
        FilesystemProfile::Native
    );
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

    FilesystemProbe::verify(&directory.0, record(MetadataStore::SidecarFiles, false)).unwrap();
}

#[test]
fn the_open_check_fails_with_efsprobe_when_the_folder_is_gone() {
    let directory = Directory::temporary();
    let missing = directory.0.join("missing");

    let error = FilesystemProbe::verify(&missing, record(MetadataStore::ExtendedAttributes, true))
        .unwrap_err();

    assert_eq!(error.code(), NativeStorageErrorCode::FilesystemProbe);
    assert!(
        error.message().contains("recorded in format.json"),
        "{error}"
    );
}

#[test]
fn no_override_always_matches_the_record() {
    let recorded = record(MetadataStore::SidecarFiles, false);

    FilesystemProbe::check_override(recorded, None).unwrap();
}

#[test]
fn the_same_override_matches_the_record() {
    let recorded = record(MetadataStore::SidecarFiles, false);

    FilesystemProbe::check_override(recorded, Some(FilesystemProfile::EfsSidecarV1)).unwrap();
}

#[test]
fn another_override_is_refused_on_an_existing_app_folder() {
    let recorded = record(MetadataStore::ExtendedAttributes, true);

    let error = FilesystemProbe::check_override(recorded, Some(FilesystemProfile::EfsSidecarV1))
        .unwrap_err();

    assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
    assert!(error.message().contains("efs-sidecar-v1"), "{error}");
    assert!(error.message().contains("xattr"), "{error}");
}

#[test]
fn a_new_app_folder_records_the_probe_in_format_json() {
    let drive = Directory::temporary();
    let layout = RootLayout::waived(drive.0.join("app"));

    let prepared = AppFolder::prepare(&layout, "26.50.02", |folder| {
        FilesystemProbe::run(folder, None)
    })
    .unwrap();
    let recorded = FormatMarker::read(&layout.format_marker_path())
        .unwrap()
        .unwrap();

    assert_eq!(
        prepared.probe().metadata(),
        MetadataStore::ExtendedAttributes
    );
    assert_eq!(recorded.probe(), prepared.probe());
}

#[test]
fn a_new_app_folder_passes_its_open_check() {
    let drive = Directory::temporary();
    let layout = RootLayout::waived(drive.0.join("app"));
    let prepared = AppFolder::prepare(&layout, "26.50.02", |folder| {
        FilesystemProbe::run(folder, None)
    })
    .unwrap();

    FilesystemProbe::verify(&layout.app_state_folder(), prepared.probe()).unwrap();
}

#[test]
fn the_probe_of_a_new_app_folder_leaves_only_fylo_files() {
    let drive = Directory::temporary();
    let layout = RootLayout::waived(drive.0.join("app"));

    AppFolder::prepare(&layout, "26.50.02", |folder| {
        FilesystemProbe::run(folder, None)
    })
    .unwrap();
    let names = names_in(&layout.app_state_folder());

    assert!(
        names.iter().all(|name| !name.contains("probe")),
        "{names:?}"
    );
}

#[test]
#[ignore = "needs FYLO_TEST_NO_XATTR_DIR on a file system without extended attributes"]
fn a_file_system_without_attributes_gets_sidecar_files() {
    let directory = Directory::without_attributes();

    let probed = FilesystemProbe::run(&directory.0, None).unwrap();

    assert_eq!(probed.metadata(), MetadataStore::SidecarFiles);
    assert_eq!(probed.max_attribute_bytes(), None);
    assert_eq!(directory.names(), Vec::<String>::new());
}

#[test]
#[ignore = "needs FYLO_TEST_NO_XATTR_DIR on a file system without extended attributes"]
fn a_new_app_folder_on_a_share_without_attributes_records_sidecar_files() {
    let drive = Directory::without_attributes();
    let layout = RootLayout::waived(drive.0.join("app"));

    let prepared = AppFolder::prepare(&layout, "26.50.02", |folder| {
        FilesystemProbe::run(folder, None)
    })
    .unwrap();

    assert_eq!(prepared.probe().metadata(), MetadataStore::SidecarFiles);
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
        record(MetadataStore::ExtendedAttributes, true),
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
