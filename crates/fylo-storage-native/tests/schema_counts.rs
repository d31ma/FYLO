//! Convergence counts (ADR 0017 section 6): the writer keeps a count of a
//! collection's live documents per stored `_v` through every commit, on the
//! write-ahead log and on the transaction journal, and a count that a crash,
//! a recovery or any other change leaves behind is detected as stale rather
//! than trusted.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_storage_native::{
    CollectionKind, FilesystemProfile, NativeCollection, NativeRoot, NativeWriteRoot,
    PutDocumentOptions, RootLease, RootOpenOptions, SchemaVersionCounts, WriteAccess,
};
use serde_json::{Map, Value, json};

const ITEMS: &str = "items";
const CHILD: &str = "FYLO_SCHEMA_COUNTS_CHILD";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    efs: bool,
    /// The main root, when this is a branch worktree.
    repository: Option<PathBuf>,
    /// The EFS profile's root lease, held from first use until released for
    /// a child process to take.
    lease: RefCell<Option<RootLease>>,
}

impl Fixture {
    /// A root whose `items` have a registry of two versions, so the writer
    /// keeps their count. Storage passes version files through unread.
    fn new(efs: bool) -> Self {
        let parent = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "fylo-schema-counts-{}-{}",
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
            repository: None,
            lease: RefCell::new(None),
        };
        let writer = fixture.writer();
        writer
            .create_collection(ITEMS, CollectionKind::Document, None)
            .unwrap();
        for ordinal in 1..=2 {
            writer
                .commit_schema_version(ITEMS, ordinal, b"{}\n", |_| Ok(()))
                .unwrap();
        }
        fixture
    }

    fn at(root: &Path, efs: bool) -> Self {
        Self {
            parent: root.parent().unwrap().to_owned(),
            root: root.to_owned(),
            efs,
            repository: None,
            lease: RefCell::new(None),
        }
    }

    /// A view of the branch worktree at `root`, checked out from this root.
    fn branch(&self, root: &Path) -> Self {
        Self {
            parent: self.parent.clone(),
            root: root.to_owned(),
            efs: self.efs,
            repository: Some(self.root.clone()),
            lease: RefCell::new(None),
        }
    }

    fn options(&self) -> RootOpenOptions {
        let mut options = RootOpenOptions::new(&self.root);
        if let Some(repository) = &self.repository {
            options = options.with_repository(repository);
        }
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

    /// Give up the root lease, so another process can own the root.
    fn release(&self) {
        self.lease.borrow_mut().take();
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(self.options()).unwrap()
    }

    fn collection(&self) -> NativeCollection {
        NativeRoot::open_profiled(self.options())
            .unwrap()
            .collection(ITEMS)
            .unwrap()
    }

    fn recorded(&self) -> Option<SchemaVersionCounts> {
        self.collection().recorded_schema_versions().unwrap()
    }

    fn scanned(&self) -> SchemaVersionCounts {
        self.collection().scan_schema_versions().unwrap()
    }

    /// The writer kept the count: it is current and it is right.
    fn assert_kept(&self, step: &str) {
        let recorded = self
            .recorded()
            .unwrap_or_else(|| panic!("{step}: the count is stale"));
        assert_eq!(recorded, self.scanned(), "{step}");
    }

    /// The count is right or reads as stale; it is never trusted wrong.
    fn assert_never_wrong(&self, step: &str) {
        if let Some(recorded) = self.recorded() {
            assert_eq!(recorded, self.scanned(), "{step}");
        }
    }

    fn counts_file(&self) -> PathBuf {
        self.root
            .join(".fylo-schemas")
            .join(ITEMS)
            .join("counts.json")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if std::env::var(CHILD).is_err() && self.repository.is_none() {
            let _ = fs::remove_dir_all(&self.parent);
        }
    }
}

fn id(number: usize) -> String {
    format!("4VRNF5{:05}", 20_000 + number)
}

fn body(version: Option<&str>, number: usize) -> Vec<u8> {
    let mut value = json!({ "number": number, "bucket": number % 3 });
    if let Some(version) = version {
        value["_v"] = json!(version);
    }
    serde_json::to_vec(&value).unwrap()
}

fn put(writer: &NativeWriteRoot, number: usize, version: Option<&str>) {
    writer
        .put_document(
            ITEMS,
            &id(number),
            &body(version, number),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn counts(versions: &[(&str, u64)], unversioned: u64) -> SchemaVersionCounts {
    SchemaVersionCounts {
        versions: versions
            .iter()
            .map(|(label, count)| ((*label).to_owned(), *count))
            .collect(),
        unversioned,
    }
}

fn changes(value: &Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

/// Every write path keeps the count, on the log or on the journal.
fn every_write_keeps_the_count(efs: bool) {
    let fixture = Fixture::new(efs);
    let writer = fixture.writer();
    put(&writer, 1, Some("v1"));
    fixture.assert_kept("first put");
    put(&writer, 2, Some("v1"));
    put(&writer, 3, None);
    put(&writer, 4, Some("v2"));
    fixture.assert_kept("puts");
    assert_eq!(fixture.scanned(), counts(&[("v1", 2), ("v2", 1)], 1));

    writer
        .patch_document(ITEMS, &id(1), &body(Some("v2"), 1), None)
        .unwrap();
    fixture.assert_kept("patch");
    writer
        .patch_document_fields(ITEMS, &id(3), &changes(&json!({"_v":"v2"})), None)
        .unwrap();
    fixture.assert_kept("patch fields");
    writer
        .patch_document_fields(ITEMS, &id(4), &changes(&json!({"bucket":9})), None)
        .unwrap();
    fixture.assert_kept("patch of another field");
    writer
        .set_record_metadata(ITEMS, &id(4), &changes(&json!({"note":"kept"})), None)
        .unwrap();
    fixture.assert_kept("metadata");
    writer.delete_document(ITEMS, &id(2), None).unwrap();
    fixture.assert_kept("delete");
    assert_eq!(fixture.scanned(), counts(&[("v2", 3)], 0));
    writer.restore_document(ITEMS, &id(2), None).unwrap();
    fixture.assert_kept("restore");

    let insert = writer
        .execute_sql_mutation(
            "INSERT INTO items (number, _v) VALUES (9, 'v1')",
            None,
            WriteAccess::default(),
        )
        .unwrap();
    assert_eq!(insert.affected, 1);
    fixture.assert_kept("SQL INSERT");
    writer
        .execute_sql_mutation(
            "UPDATE items SET _v = 'v2' WHERE bucket = 2",
            None,
            WriteAccess::default(),
        )
        .unwrap();
    fixture.assert_kept("SQL UPDATE");
    writer
        .execute_sql_mutation(
            "DELETE FROM items WHERE _v = 'v1'",
            None,
            WriteAccess::default(),
        )
        .unwrap();
    fixture.assert_kept("SQL DELETE");
    assert_eq!(fixture.scanned(), counts(&[("v2", 4)], 0));

    writer.reshard_collection(ITEMS, 3).unwrap();
    fixture.assert_kept("reshard");
    writer.rebuild_collection(ITEMS).unwrap();
    fixture.assert_kept("rebuild");
    // An explicit recovery checkpoints the log away.
    writer.recover_collection(ITEMS).unwrap();
    fixture.assert_kept("checkpoint");
}

#[test]
fn every_logged_write_keeps_the_count() {
    every_write_keeps_the_count(false);
}

#[cfg(unix)]
#[test]
fn every_journaled_write_keeps_the_count() {
    every_write_keeps_the_count(true);
}

#[test]
fn a_count_behind_the_generation_or_unreadable_is_stale_and_rebuilt() {
    let fixture = Fixture::new(false);
    let writer = fixture.writer();
    put(&writer, 1, Some("v1"));
    put(&writer, 2, Some("v2"));
    fixture.assert_kept("seeded");

    // A count stamped at another generation is not trusted.
    let mut record: Value =
        serde_json::from_slice(&fs::read(fixture.counts_file()).unwrap()).unwrap();
    record["generation"] = json!(record["generation"].as_u64().unwrap() - 2);
    record["versions"] = json!({"v1": 7});
    fs::write(fixture.counts_file(), serde_json::to_vec(&record).unwrap()).unwrap();
    assert_eq!(fixture.recorded(), None);
    // The next commit counts again by scan before it applies its change.
    put(&writer, 3, Some("v1"));
    fixture.assert_kept("after a stale count");
    assert_eq!(fixture.scanned(), counts(&[("v1", 2), ("v2", 1)], 0));

    // A torn or missing count is the same.
    fs::write(fixture.counts_file(), b"{\"format\":").unwrap();
    assert_eq!(fixture.recorded(), None);
    fs::remove_file(fixture.counts_file()).unwrap();
    assert_eq!(fixture.recorded(), None);
    assert_eq!(
        writer.rebuild_schema_versions(ITEMS).unwrap(),
        counts(&[("v1", 2), ("v2", 1)], 0)
    );
    fixture.assert_kept("rebuilt");

    // A collection whose registry has one version is not tracked.
    writer
        .create_collection("single", CollectionKind::Document, None)
        .unwrap();
    writer
        .commit_schema_version("single", 1, b"{}\n", |_| Ok(()))
        .unwrap();
    writer
        .put_document(
            "single",
            &id(1),
            &body(Some("v1"), 1),
            PutDocumentOptions::default(),
        )
        .unwrap();
    assert!(
        !fixture
            .root
            .join(".fylo-schemas/single/counts.json")
            .exists()
    );
}

/// Run `operation` in a child process that dies at `failpoint`.
fn run_crash_child(fixture: &Fixture, operation: &str, failpoint: &str) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, operation)
        .env("FYLO_SCHEMA_COUNTS_ROOT", &fixture.root)
        .env(
            "FYLO_SCHEMA_COUNTS_EFS",
            if fixture.efs { "1" } else { "0" },
        )
        .env("FYLO_RUST_FAILPOINT", failpoint)
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .status()
        .unwrap();
    assert!(!status.success(), "{operation} did not stop at {failpoint}");
}

/// The other half of [`run_crash_child`]: does nothing in an ordinary run.
#[test]
fn crash_child() {
    let Ok(operation) = std::env::var(CHILD) else {
        return;
    };
    let root = PathBuf::from(std::env::var("FYLO_SCHEMA_COUNTS_ROOT").unwrap());
    let efs = std::env::var("FYLO_SCHEMA_COUNTS_EFS").unwrap() == "1";
    let fixture = Fixture::at(&root, efs);
    let writer = fixture.writer();
    match operation.as_str() {
        "patch" => writer
            .patch_document(ITEMS, &id(1), &body(Some("v2"), 1), None)
            .unwrap(),
        "delete" => writer.delete_document(ITEMS, &id(2), None).unwrap(),
        other => panic!("unknown child operation {other}"),
    }
    // Not reached: the failpoint aborts the process.
}

fn crash_and_recover(efs: bool, operation: &str, failpoint: &str) {
    let fixture = Fixture::new(efs);
    {
        let writer = fixture.writer();
        put(&writer, 1, Some("v1"));
        put(&writer, 2, Some("v1"));
        put(&writer, 3, None);
        writer.recover_collection(ITEMS).unwrap();
    }
    fixture.assert_kept("before the crash");
    fixture.release();
    run_crash_child(&fixture, operation, failpoint);
    let step = format!("{operation} stopped at {failpoint}");
    // Until recovery a reader sees a write in progress or a stale count.
    let collection = fixture.collection();
    if let Ok(Some(recorded)) = collection.recorded_schema_versions() {
        assert_eq!(
            recorded,
            collection.scan_schema_versions().unwrap(),
            "{step}"
        );
    }
    let writer = fixture.writer();
    writer.recover_collection(ITEMS).unwrap();
    fixture.assert_never_wrong(&format!("{step}, recovered"));
    // The next commit counts again and keeps the count from there.
    put(&writer, 4, Some("v2"));
    fixture.assert_kept(&format!("{step}, next write"));
}

#[test]
fn a_crash_on_the_log_leaves_the_count_right_or_stale() {
    for failpoint in ["after-log-append", "after-log-sync", "after-log-apply-step"] {
        crash_and_recover(false, "patch", failpoint);
        crash_and_recover(false, "delete", failpoint);
    }
}

#[cfg(unix)]
#[test]
fn a_crash_on_the_journal_leaves_the_count_right_or_stale() {
    for failpoint in [
        "after-capture",
        "before-commit-marker",
        "after-commit-marker",
    ] {
        crash_and_recover(true, "patch", failpoint);
        crash_and_recover(true, "delete", failpoint);
    }
}

#[test]
fn restore_checkout_and_merge_leave_each_worktree_counted() {
    let fixture = Fixture::new(false);
    let writer = fixture.writer();
    put(&writer, 1, Some("v1"));
    put(&writer, 2, Some("v1"));
    let first = writer.commit_if_dirty("two at v1").unwrap().unwrap();
    writer
        .patch_document(ITEMS, &id(1), &body(Some("v2"), 1), None)
        .unwrap();
    put(&writer, 3, None);
    writer
        .commit_if_dirty("one moved, one unversioned")
        .unwrap();
    assert_eq!(fixture.scanned(), counts(&[("v1", 1), ("v2", 1)], 1));
    fixture.assert_kept("before the restore");

    // Materializing a commit replaces the documents under the count.
    writer.restore_repository_commit(&first, true).unwrap();
    fixture.assert_kept("restored");
    assert_eq!(fixture.scanned(), counts(&[("v1", 2)], 0));

    // A branch shares the registry but keeps its own count.
    let main = writer.repository_branches().unwrap()["current"]
        .as_str()
        .unwrap()
        .to_owned();
    let checkout = writer.checkout_repository("feature", true).unwrap();
    let branch = fixture.branch(Path::new(checkout["root"].as_str().unwrap()));
    let branch_writer = branch.writer();
    put(&branch_writer, 4, Some("v2"));
    branch.assert_kept("branch write");
    assert_eq!(branch.scanned(), counts(&[("v1", 2), ("v2", 1)], 0));
    fixture.assert_kept("main after a branch write");
    assert_eq!(fixture.scanned(), counts(&[("v1", 2)], 0));
    let files = fs::read_dir(fixture.counts_file().parent().unwrap())
        .unwrap()
        .filter(|entry| {
            let name = entry.as_ref().unwrap().file_name();
            let name = name.to_string_lossy();
            name.starts_with("counts.") && name.ends_with(".json")
        })
        .count();
    assert_eq!(files, 2, "one count per worktree");
    branch_writer.commit_if_dirty("one at v2").unwrap().unwrap();

    writer.checkout_repository(&main, false).unwrap();
    writer.merge_repository("feature", None).unwrap();
    fixture.assert_kept("merged");
    assert_eq!(fixture.scanned(), counts(&[("v1", 2), ("v2", 1)], 0));
}
