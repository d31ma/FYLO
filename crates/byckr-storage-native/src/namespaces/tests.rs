use super::NamespaceAdmin;
use crate::layout::{NamespaceName, RootLayout};
use crate::{FilesystemProfile, LeaseTiming, NativeStorageErrorCode, RootLease};
use std::path::PathBuf;
use std::time::Duration;

struct AppFixture {
    drive: PathBuf,
    layout: RootLayout,
}

impl AppFixture {
    const CREATED_BY: &str = "test";

    fn new() -> Self {
        let drive = byckr_test_support::TestFolder::path()
            .join(crate::write::unique_name("fylo-namespaces"));
        std::fs::create_dir_all(&drive).unwrap();
        let drive = std::fs::canonicalize(drive).unwrap();
        let layout = RootLayout::checked(drive.join("chat")).unwrap();
        Self { drive, layout }
    }

    fn name(text: &str) -> NamespaceName {
        NamespaceName::parse(text).unwrap()
    }

    fn create(&self, text: &str) -> PathBuf {
        NamespaceAdmin::create_namespace(&self.layout, &Self::name(text), Self::CREATED_BY, None)
            .unwrap()
    }

    fn settings() -> (FilesystemProfile, LeaseTiming) {
        let timing = LeaseTiming::new(Duration::from_millis(20), Duration::from_secs(2)).unwrap();
        (FilesystemProfile::Native, timing)
    }

    fn drop_namespace(&self, text: &str, cascade: bool) -> Result<(), crate::NativeStorageError> {
        NamespaceAdmin::drop_namespace(&self.layout, &Self::name(text), cascade, Self::settings())
    }

    fn listed(&self) -> Vec<String> {
        NamespaceAdmin::list_namespaces(&self.layout)
            .unwrap()
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect()
    }

    fn trash_entries(&self) -> usize {
        std::fs::read_dir(self.layout.trash_folder())
            .unwrap()
            .count()
    }
}

impl Drop for AppFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.drive);
    }
}

#[test]
fn a_created_namespace_has_its_folder_state_marker_and_guard_files() {
    let fixture = AppFixture::new();
    let folder = fixture.create("billing");
    assert!(folder.join(".fylo").is_dir());
    assert!(folder.join(".fylo/namespace.json").is_file());
    for guard in [".collections", ".buckets", ".fylo-catalog"] {
        assert!(folder.join(guard).is_file(), "{guard} missing");
    }
    assert!(fixture.layout.format_marker_path().is_file());
}

#[test]
fn namespaces_are_listed_by_name() {
    let fixture = AppFixture::new();
    fixture.create("slice-2");
    fixture.create("billing");
    fixture.create("public");
    assert_eq!(fixture.listed(), ["billing", "public", "slice-2"]);
}

#[test]
fn creating_an_existing_namespace_is_refused() {
    let fixture = AppFixture::new();
    fixture.create("billing");
    let refused = NamespaceAdmin::create_namespace(
        &fixture.layout,
        &AppFixture::name("billing"),
        AppFixture::CREATED_BY,
        None,
    )
    .unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::NamespaceExists);
}

#[test]
fn a_create_a_crash_stopped_before_its_marker_is_finished_by_the_next_create() {
    let fixture = AppFixture::new();
    fixture.create("other");
    std::fs::create_dir(
        fixture
            .layout
            .namespace_folder(&AppFixture::name("billing")),
    )
    .unwrap();
    assert_eq!(fixture.listed(), ["other"]);
    fixture.create("billing");
    assert_eq!(fixture.listed(), ["billing", "other"]);
}

#[test]
fn an_empty_namespace_is_dropped_and_leaves_nothing_in_the_trash() {
    let fixture = AppFixture::new();
    let folder = fixture.create("slice-1");
    fixture.drop_namespace("slice-1", false).unwrap();
    assert!(!folder.exists());
    assert!(fixture.listed().is_empty());
    assert_eq!(fixture.trash_entries(), 0);
}

#[test]
fn a_namespace_with_collections_is_dropped_only_with_cascade() {
    let fixture = AppFixture::new();
    let folder = fixture.create("slice-1");
    std::fs::create_dir(folder.join("users")).unwrap();
    let refused = fixture.drop_namespace("slice-1", false).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::NamespaceNotEmpty);
    assert!(folder.join("users").is_dir());
    fixture.drop_namespace("slice-1", true).unwrap();
    assert!(!folder.exists());
}

#[test]
fn dropping_a_missing_namespace_is_reported_as_not_found() {
    let fixture = AppFixture::new();
    fixture.create("public");
    let refused = fixture.drop_namespace("slice-9", false).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::NotFound);
}

#[test]
fn a_namespace_in_use_is_not_dropped() {
    let fixture = AppFixture::new();
    let folder = fixture.create("slice-1");
    let (profile, timing) = AppFixture::settings();
    let lease = RootLease::acquire_namespace(
        &fixture.layout,
        &AppFixture::name("slice-1"),
        profile,
        timing,
    )
    .unwrap();
    let refused = fixture.drop_namespace("slice-1", true).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::RootLocked);
    assert!(folder.is_dir());
    drop(lease);
    fixture.drop_namespace("slice-1", true).unwrap();
}

#[test]
fn two_namespaces_of_one_app_are_owned_at_the_same_time() {
    let fixture = AppFixture::new();
    fixture.create("slice-1");
    fixture.create("slice-2");
    let (profile, timing) = AppFixture::settings();
    let first = RootLease::acquire_namespace(
        &fixture.layout,
        &AppFixture::name("slice-1"),
        profile,
        timing,
    )
    .unwrap();
    let second = RootLease::acquire_namespace(
        &fixture.layout,
        &AppFixture::name("slice-2"),
        profile,
        timing,
    )
    .unwrap();
    let again = RootLease::acquire_namespace(
        &fixture.layout,
        &AppFixture::name("slice-1"),
        profile,
        timing,
    )
    .unwrap_err();
    assert_eq!(again.code(), NativeStorageErrorCode::RootLocked);
    assert_eq!(
        first.root(),
        fixture
            .layout
            .namespace_folder(&AppFixture::name("slice-1"))
    );
    first.assert_owned().unwrap();
    second.assert_owned().unwrap();
    assert!(
        fixture
            .layout
            .lease_record_path(&AppFixture::name("slice-1"))
            .is_file()
    );
    assert!(
        fixture
            .layout
            .lease_record_path(&AppFixture::name("slice-2"))
            .is_file()
    );
}

#[test]
fn a_lease_on_a_missing_namespace_is_reported_as_not_found() {
    let fixture = AppFixture::new();
    fixture.create("public");
    let (profile, timing) = AppFixture::settings();
    let refused = RootLease::acquire_namespace(
        &fixture.layout,
        &AppFixture::name("slice-9"),
        profile,
        timing,
    )
    .unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::NotFound);
}

#[test]
fn a_folder_a_crash_left_in_the_trash_is_removed_by_the_next_list() {
    let fixture = AppFixture::new();
    fixture.create("public");
    let leftover = fixture.layout.trash_folder().join("slice-1-crashed");
    std::fs::create_dir_all(leftover.join("users")).unwrap();
    assert_eq!(fixture.listed(), ["public"]);
    assert_eq!(fixture.trash_entries(), 0);
}

#[test]
fn a_namespace_can_be_created_again_after_a_drop() {
    let fixture = AppFixture::new();
    fixture.create("slice-1");
    fixture.drop_namespace("slice-1", false).unwrap();
    fixture.create("slice-1");
    let (profile, timing) = AppFixture::settings();
    let lease = RootLease::acquire_namespace(
        &fixture.layout,
        &AppFixture::name("slice-1"),
        profile,
        timing,
    )
    .unwrap();
    lease.assert_owned().unwrap();
}
