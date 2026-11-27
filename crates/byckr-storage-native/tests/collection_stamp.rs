//! Collection stamps (ADR 0030 section 3): every change a cache must not
//! miss gives a collection another stamp, and only a stable files-layout
//! collection that can be told apart from another life of itself is
//! cacheable.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use byckr_storage_native::{
    CollectionKind, CollectionLayout, CollectionStamp, FilesystemProfile, NativeRoot,
    NativeWriteRoot, PutDocumentOptions, RootLease, RootOpenOptions,
};
use serde_json::json;

const ITEMS: &str = "items";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    efs: bool,
    lease: RefCell<Option<RootLease>>,
}

impl Fixture {
    fn native() -> Self {
        Self::new(false)
    }

    #[cfg(unix)]
    fn efs() -> Self {
        Self::new(true)
    }

    fn new(efs: bool) -> Self {
        let parent = byckr_test_support::TestFolder::path()
            .canonicalize()
            .unwrap()
            .join(format!(
                "fylo-collection-stamp-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_dir_all(&parent);
        let root = parent.join("root");
        fs::create_dir_all(&root).unwrap();
        let fixture = Self {
            parent,
            root,
            efs,
            lease: RefCell::new(None),
        };
        fixture
            .writer()
            .create_collection(ITEMS, CollectionKind::Document, None)
            .unwrap();
        fixture
    }

    fn options(&self) -> RootOpenOptions {
        let options = RootOpenOptions::new(&self.root);
        if !self.efs {
            return options;
        }
        let lease = self
            .lease
            .borrow_mut()
            .get_or_insert_with(|| {
                RootLease::acquire_with_profile(&self.root, FilesystemProfile::EfsSidecarV1)
                    .unwrap()
            })
            .clone();
        options
            .with_profile(FilesystemProfile::EfsSidecarV1)
            .with_lease(lease)
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(self.options()).unwrap()
    }

    fn stamp(&self) -> CollectionStamp {
        NativeRoot::open_profiled(self.options())
            .unwrap()
            .collection(ITEMS)
            .unwrap()
            .collection_stamp()
            .unwrap()
    }

    fn put(&self, number: usize) {
        self.writer()
            .put_document(
                ITEMS,
                &id(number),
                &body(number),
                PutDocumentOptions::default(),
            )
            .unwrap();
    }

    fn state_file(&self) -> PathBuf {
        self.root
            .join(".fylo-transactions")
            .join(".collections")
            .join(ITEMS)
            .join("state.json")
    }

    fn incarnation_file(&self) -> PathBuf {
        self.state_file().with_file_name("incarnation")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.lease.borrow_mut().take();
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn id(number: usize) -> String {
    format!("4VRNF5{:05}", 20_000 + number)
}

fn body(number: usize) -> Vec<u8> {
    serde_json::to_vec(&json!({ "number": number })).unwrap()
}

#[test]
fn a_new_collection_is_cacheable_by_its_incarnation() {
    let fixture = Fixture::native();
    let stamp = fixture.stamp();
    assert!(stamp.incarnation().is_known());
    assert!(stamp.is_cacheable());
}

#[test]
fn an_unchanged_collection_keeps_its_stamp() {
    let fixture = Fixture::native();
    fixture.put(1);
    assert_eq!(fixture.stamp(), fixture.stamp());
}

#[test]
fn an_inserted_document_changes_the_stamp() {
    let fixture = Fixture::native();
    fixture.put(1);
    let before = fixture.stamp();
    fixture.put(2);
    assert_ne!(before, fixture.stamp());
}

#[test]
fn a_changed_document_changes_the_stamp() {
    let fixture = Fixture::native();
    fixture.put(1);
    let before = fixture.stamp();
    fixture
        .writer()
        .patch_document(
            ITEMS,
            &id(1),
            &serde_json::to_vec(&json!({ "number": 7 })).unwrap(),
        )
        .unwrap();
    assert_ne!(before, fixture.stamp());
}

#[test]
fn a_deleted_document_changes_the_stamp() {
    let fixture = Fixture::native();
    fixture.put(1);
    let before = fixture.stamp();
    fixture.writer().delete_document(ITEMS, &id(1)).unwrap();
    assert_ne!(before, fixture.stamp());
}

#[test]
fn a_collection_created_again_has_another_incarnation_and_stamp() {
    let fixture = Fixture::native();
    fixture.put(1);
    let before = fixture.stamp();
    let writer = fixture.writer();
    writer.drop_collection(ITEMS).unwrap();
    writer
        .create_collection(ITEMS, CollectionKind::Document, None)
        .unwrap();
    let after = fixture.stamp();
    assert_ne!(before.incarnation(), after.incarnation());
    assert_ne!(before, after);
}

#[test]
fn the_same_state_written_again_by_rename_changes_the_stamp() {
    let fixture = Fixture::native();
    fixture.put(1);
    let before = fixture.stamp();
    let bytes = fs::read(fixture.state_file()).unwrap();
    let copy = fixture.state_file().with_file_name("state.json.copy");
    fs::write(&copy, &bytes).unwrap();
    fs::rename(&copy, fixture.state_file()).unwrap();
    let after = fixture.stamp();
    assert_eq!(before.state(), after.state());
    assert_ne!(before, after);
}

#[test]
fn the_same_state_written_again_in_place_changes_the_stamp() {
    let fixture = Fixture::native();
    fixture.put(1);
    let before = fixture.stamp();
    let bytes = fs::read(fixture.state_file()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(fixture.state_file(), &bytes).unwrap();
    let after = fixture.stamp();
    assert_eq!(before.state(), after.state());
    assert_ne!(before, after);
}

#[test]
fn restoring_a_commit_changes_the_stamp() {
    let fixture = Fixture::native();
    fixture.put(1);
    let writer = fixture.writer();
    let first = writer.commit_if_dirty("one").unwrap().unwrap();
    fixture.put(2);
    writer.commit_if_dirty("two").unwrap().unwrap();
    let before = fixture.stamp();
    writer.restore_repository_commit(&first, true).unwrap();
    assert_ne!(before, fixture.stamp());
}

#[test]
fn rebuilding_the_index_changes_the_stamp() {
    let fixture = Fixture::native();
    fixture.put(1);
    let before = fixture.stamp();
    fixture.writer().rebuild_collection(ITEMS).unwrap();
    assert_ne!(before, fixture.stamp());
}

#[test]
fn resharding_changes_the_stamp() {
    let fixture = Fixture::native();
    fixture.put(1);
    let before = fixture.stamp();
    fixture.writer().reshard_collection(ITEMS, 2).unwrap();
    assert_ne!(before, fixture.stamp());
}

#[test]
fn a_collection_without_its_incarnation_is_cacheable_by_its_state_file() {
    let fixture = Fixture::native();
    fixture.put(1);
    let before = fixture.stamp();
    fs::remove_file(fixture.incarnation_file()).unwrap();
    let after = fixture.stamp();
    assert!(!after.incarnation().is_known());
    assert!(after.is_cacheable());
    assert_ne!(before, after);
}

#[test]
fn a_collection_with_neither_file_is_not_cacheable() {
    let fixture = Fixture::native();
    let _ = fs::remove_file(fixture.state_file());
    fs::remove_file(fixture.incarnation_file()).unwrap();
    assert!(!fixture.stamp().is_cacheable());
}

#[test]
fn a_damaged_incarnation_reads_as_unknown() {
    let fixture = Fixture::native();
    fs::write(fixture.incarnation_file(), b"FYLN0000000000000").unwrap();
    assert!(!fixture.stamp().incarnation().is_known());
}

#[test]
fn a_segment_collection_is_not_cacheable() {
    let fixture = Fixture::native();
    fixture
        .writer()
        .create_collection_with_layout(
            "segmented",
            CollectionKind::Document,
            None,
            Some(CollectionLayout::Segments),
        )
        .unwrap();
    let stamp = NativeRoot::open_profiled(fixture.options())
        .unwrap()
        .collection("segmented")
        .unwrap()
        .collection_stamp()
        .unwrap();
    assert!(!stamp.is_cacheable());
}

/// The EFS profile has no Windows form (ADR 0029).
#[cfg(unix)]
#[test]
fn an_efs_collection_is_cacheable_and_changes_with_a_write() {
    let fixture = Fixture::efs();
    fixture.put(1);
    let before = fixture.stamp();
    assert!(before.is_cacheable());
    assert_eq!(before, fixture.stamp());
    fixture.put(2);
    assert_ne!(before, fixture.stamp());
}

/// The EFS profile has no Windows form (ADR 0029).
#[cfg(unix)]
#[test]
fn an_efs_state_written_again_by_rename_changes_the_stamp() {
    let fixture = Fixture::efs();
    fixture.put(1);
    let before = fixture.stamp();
    let bytes = fs::read(fixture.state_file()).unwrap();
    let copy = fixture.state_file().with_file_name("state.json.copy");
    fs::write(&copy, &bytes).unwrap();
    fs::rename(&copy, fixture.state_file()).unwrap();
    assert_ne!(before, fixture.stamp());
}

/// The EFS profile has no Windows form (ADR 0029).
#[cfg(unix)]
#[test]
fn an_efs_collection_created_again_has_another_incarnation() {
    let fixture = Fixture::efs();
    fixture.put(1);
    let before = fixture.stamp();
    let writer = fixture.writer();
    writer.drop_collection(ITEMS).unwrap();
    writer
        .create_collection(ITEMS, CollectionKind::Document, None)
        .unwrap();
    assert_ne!(before.incarnation(), fixture.stamp().incarnation());
}
