use std::fs;
use std::path::{Path, PathBuf};

use crate::NativeStorageError;

use super::{
    AppFolder, FormatMarker, GuardFiles, MetadataStore, NamespaceMarker, NamespaceName,
    ProbeRecord, RootLayout,
};

struct TestDrive(PathBuf);

impl TestDrive {
    fn new() -> Self {
        let folder = std::env::temp_dir().join(crate::write::unique_name("fylo-layout-test"));
        fs::create_dir_all(&folder).expect("create the test drive folder");
        Self(folder)
    }

    fn layout(&self, app: &str) -> RootLayout {
        RootLayout::checked(self.0.join(app)).expect("a valid app name")
    }
}

impl Drop for TestDrive {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn probe() -> ProbeRecord {
    ProbeRecord::new(MetadataStore::ExtendedAttributes, true)
}

fn marker(created_by: &str) -> FormatMarker {
    FormatMarker::new(created_by, probe()).expect("a valid marker")
}

fn prepare(layout: &RootLayout, created_by: &str) -> Result<FormatMarker, NativeStorageError> {
    AppFolder::prepare(layout, created_by, |_| Ok(probe()))
}

fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .expect("list the folder")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn a_new_app_folder_gets_its_marker_guards_and_trash() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    let recorded = prepare(&layout, "26.50.02").unwrap();
    assert_eq!(recorded, marker("26.50.02"));
    assert_eq!(
        names_in(layout.app_folder()),
        [".buckets", ".collections", ".fylo", ".fylo-catalog"]
    );
    assert_eq!(
        names_in(&layout.app_state_folder()),
        ["format.json", "trash"]
    );
    assert!(GuardFiles::are_in_place(layout.app_folder()).unwrap());
    let on_disk = FormatMarker::read(&layout.format_marker_path()).unwrap();
    assert_eq!(on_disk, Some(marker("26.50.02")));
    for name in GuardFiles::NAMES {
        let metadata = fs::symlink_metadata(layout.app_folder().join(name)).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.len(), 0);
    }
}

#[test]
fn an_existing_app_folder_keeps_its_recorded_marker() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    prepare(&layout, "26.50.02").unwrap();
    let recorded = prepare(&layout, "27.01.01").unwrap();
    assert_eq!(recorded.created_by(), "26.50.02");
}

#[test]
fn an_empty_existing_folder_becomes_an_app_folder() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    fs::create_dir(layout.app_folder()).unwrap();
    assert!(prepare(&layout, "26.50.02").is_ok());
}

#[test]
fn a_folder_byckr_did_not_make_is_refused_and_left_alone() {
    let drive = TestDrive::new();
    let layout = drive.layout("blobs");
    fs::create_dir(layout.app_folder()).unwrap();
    fs::write(layout.app_folder().join("photo.jpg"), b"bytes").unwrap();
    let error = prepare(&layout, "26.50.02").unwrap_err();
    assert_eq!(error.code().as_str(), "ENOTFYLOROOT");
    assert!(
        error.message().contains("not a FYLO app folder"),
        "{}",
        error.message()
    );
    assert_eq!(names_in(layout.app_folder()), ["photo.jpg"]);
}

#[test]
fn an_old_layout_root_is_refused_and_left_alone() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    fs::create_dir_all(layout.app_folder().join(".collections/users/docs")).unwrap();
    let error = prepare(&layout, "26.50.02").unwrap_err();
    assert_eq!(error.code().as_str(), "ENOTFYLOROOT");
    assert!(
        error.message().contains("fylo upgrade-root"),
        "{}",
        error.message()
    );
    assert_eq!(names_in(layout.app_folder()), [".collections"]);
}

#[test]
fn a_crash_before_the_marker_is_finished_on_the_next_prepare() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    fs::create_dir_all(layout.app_state_folder()).unwrap();
    fs::write(layout.app_folder().join(".collections"), b"").unwrap();
    let recorded = prepare(&layout, "26.50.02").unwrap();
    assert_eq!(recorded.created_by(), "26.50.02");
    assert!(GuardFiles::are_in_place(layout.app_folder()).unwrap());
}

#[test]
fn a_crash_after_the_marker_gets_its_guards_on_the_next_prepare() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    fs::create_dir_all(layout.app_state_folder()).unwrap();
    marker("26.50.02")
        .write(&layout.format_marker_path())
        .unwrap();
    assert!(!GuardFiles::are_in_place(layout.app_folder()).unwrap());
    prepare(&layout, "26.50.02").unwrap();
    assert!(GuardFiles::are_in_place(layout.app_folder()).unwrap());
    assert!(layout.trash_folder().is_dir());
}

#[test]
fn a_guard_name_that_is_a_folder_is_refused() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    fs::create_dir_all(layout.app_state_folder()).unwrap();
    marker("26.50.02")
        .write(&layout.format_marker_path())
        .unwrap();
    fs::create_dir(layout.app_folder().join(".buckets")).unwrap();
    let error = prepare(&layout, "26.50.02").unwrap_err();
    assert_eq!(error.code().as_str(), "ENATIVE_WRONG_TYPE");
    assert!(
        error.message().contains("old layout"),
        "{}",
        error.message()
    );
}

#[test]
fn a_missing_drive_is_not_made() {
    let drive = TestDrive::new();
    let layout = RootLayout::checked(drive.0.join("missing").join("chat")).unwrap();
    let error = prepare(&layout, "26.50.02").unwrap_err();
    assert_eq!(error.code().as_str(), "ENATIVE_IO");
    assert!(!drive.0.join("missing").exists());
}

#[test]
fn an_app_path_that_is_a_file_is_refused() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    fs::write(layout.app_folder(), b"").unwrap();
    let error = prepare(&layout, "26.50.02").unwrap_err();
    assert_eq!(error.code().as_str(), "ENATIVE_WRONG_TYPE");
}

#[test]
fn a_state_folder_that_is_a_file_is_refused() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    fs::create_dir(layout.app_folder()).unwrap();
    fs::write(layout.app_state_folder(), b"").unwrap();
    let error = prepare(&layout, "26.50.02").unwrap_err();
    assert_eq!(error.code().as_str(), "ENOTFYLOROOT");
}

#[test]
fn guard_files_in_a_namespace_folder() {
    let drive = TestDrive::new();
    let folder = drive
        .layout("chat")
        .namespace_folder(&NamespaceName::public());
    fs::create_dir_all(&folder).unwrap();
    assert!(!GuardFiles::are_in_place(&folder).unwrap());
    GuardFiles::write(&folder).unwrap();
    GuardFiles::write(&folder).unwrap();
    assert!(GuardFiles::are_in_place(&folder).unwrap());
    assert!(GuardFiles::is_guard_name(".fylo-catalog"));
    assert!(!GuardFiles::is_guard_name("docs"));
}

#[test]
fn a_missing_format_marker_reads_as_none() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    assert_eq!(
        FormatMarker::read(&layout.format_marker_path()).unwrap(),
        None
    );
}

#[test]
fn a_damaged_format_marker_is_refused() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    fs::create_dir_all(layout.app_state_folder()).unwrap();
    fs::write(layout.format_marker_path(), b"{not json").unwrap();
    let error = FormatMarker::read(&layout.format_marker_path()).unwrap_err();
    assert_eq!(error.code().as_str(), "ENATIVE_METADATA");
    let error = prepare(&layout, "26.50.02").unwrap_err();
    assert_eq!(error.code().as_str(), "ENATIVE_METADATA");
}

#[test]
fn an_oversized_format_marker_is_refused() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    fs::create_dir_all(layout.app_state_folder()).unwrap();
    fs::write(layout.format_marker_path(), vec![b' '; 5000]).unwrap();
    let error = FormatMarker::read(&layout.format_marker_path()).unwrap_err();
    assert_eq!(error.code().as_str(), "ENATIVE_METADATA");
}

#[cfg(unix)]
#[test]
fn a_linked_format_marker_is_refused() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    fs::create_dir_all(layout.app_state_folder()).unwrap();
    let elsewhere = drive.0.join("elsewhere.json");
    marker("26.50.02").write(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, layout.format_marker_path()).unwrap();
    let error = FormatMarker::read(&layout.format_marker_path()).unwrap_err();
    assert_eq!(error.code().as_str(), "ENATIVE_METADATA");
}

#[test]
fn a_namespace_marker_round_trips() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    let namespace = NamespaceName::parse("slice-12").unwrap();
    fs::create_dir_all(layout.namespace_state_folder(&namespace)).unwrap();
    let path = layout.namespace_marker_path(&namespace);
    assert_eq!(NamespaceMarker::read(&path).unwrap(), None);
    NamespaceMarker::new(namespace.clone())
        .write(&path)
        .unwrap();
    let read = NamespaceMarker::read(&path).unwrap().unwrap();
    assert_eq!(read.name(), &namespace);
}

#[test]
fn the_probe_runs_in_the_state_folder_only_for_a_new_app_folder() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    let mut probed = Vec::new();
    AppFolder::prepare(&layout, "26.50.02", |folder| {
        probed.push(folder.to_path_buf());
        assert!(folder.is_dir());
        Ok(ProbeRecord::new(MetadataStore::SidecarFiles, false))
    })
    .unwrap();
    assert_eq!(probed, [layout.app_state_folder()]);
    let recorded = AppFolder::prepare(&layout, "26.50.02", |_| {
        panic!("an existing app folder is not probed again")
    })
    .unwrap();
    assert_eq!(recorded.probe().metadata(), MetadataStore::SidecarFiles);
}

#[test]
fn a_failed_probe_writes_no_marker() {
    let drive = TestDrive::new();
    let layout = drive.layout("chat");
    let error = AppFolder::prepare(&layout, "26.50.02", |_| {
        Err(NativeStorageError::new(
            crate::NativeStorageErrorCode::Unsupported,
            "probe failed",
        ))
    })
    .unwrap_err();
    assert_eq!(error.code().as_str(), "ENATIVE_UNSUPPORTED");
    assert_eq!(
        FormatMarker::read(&layout.format_marker_path()).unwrap(),
        None
    );
    assert!(prepare(&layout, "26.50.02").is_ok());
}
