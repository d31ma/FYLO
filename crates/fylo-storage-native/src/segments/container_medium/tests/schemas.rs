//! Schema versions in a `.fxdb` file (ADR 0017 section 3): schema records
//! in the collection's segments, the schemas snapshot as a `schemas` run
//! the checkpoint slot names, and the superblock's read-only-compatible
//! schemas feature. The crash tests spawn this test binary again with a
//! failpoint, running [`a_schema_child_process`].

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use fylo_format::{ContainerCheckpointSlot, ContainerExtentKind, ContainerSuperblock};

use super::{Fixture, identifier, patch, put, put_into};
use crate::segments::{SegmentMedium, SnapshotStamp};
use crate::write::SegmentSchemas;
use crate::{CollectionKind, ContainerFile, NativeRoot, NativeStorageErrorCode, NativeWriteRoot};

const SCHEMAS: ContainerExtentKind = ContainerExtentKind::Schemas;
const ROLE: &str = "FYLO_FXDB_SCHEMA_CHILD_ROLE";
const DATABASE: &str = "FYLO_FXDB_SCHEMA_CHILD_DATABASE";
const CHILD_TEST: &str = "segments::container_medium::tests::schemas::a_schema_child_process";

/// A `.fxdb` database root holding the document collections `items` and
/// `plain`.
struct Database(PathBuf);

impl Database {
    fn create(label: &str) -> Self {
        let directory = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&directory).unwrap();
        let database = Self(std::fs::canonicalize(directory).unwrap());
        NativeRoot::create_database(database.path()).unwrap();
        let writer = database.writer();
        for name in ["items", "plain"] {
            writer
                .create_collection(name, CollectionKind::Document, None)
                .unwrap();
        }
        database
    }

    fn path(&self) -> PathBuf {
        self.0.join("db.fxdb")
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(self.path()).unwrap()
    }

    fn root(&self) -> NativeRoot {
        NativeRoot::open(self.path()).unwrap()
    }

    fn medium(&self) -> std::sync::Arc<dyn SegmentMedium> {
        self.root()
            .collection("items")
            .unwrap()
            .segment_medium()
            .unwrap()
    }

    fn head(&self, collection: &str) -> Option<u32> {
        self.root().schema_registry_head(collection).unwrap()
    }

    fn commit(&self, ordinal: u32) -> Result<(), crate::NativeStorageError> {
        self.writer()
            .commit_schema_version("items", ordinal, version(ordinal).as_bytes(), |_| Ok(()))
    }

    /// Forget everything this process holds of the database, as a new
    /// process would start.
    fn reopen(&self) {
        NativeRoot::close_database(self.path()).unwrap();
    }

    /// The file as a reader opened now sees it.
    fn file(&self) -> ContainerFile {
        ContainerFile::open_reader(&self.path()).unwrap()
    }

    /// The newest checkpoint slot of `collection`.
    fn checkpoint(&self, collection: &str) -> Option<ContainerCheckpointSlot> {
        let file = self.file();
        let block = file
            .collections()
            .iter()
            .find(|held| held.header.name == collection && !held.header.dropped)
            .unwrap()
            .block;
        file.read_checkpoint(block).unwrap()
    }

    /// The `schemas` extents the map shows for any collection.
    fn schemas_extents(&self) -> usize {
        let file = self.file();
        file.collections()
            .iter()
            .map(|held| file.allocator().owned(SCHEMAS, held.header.id).len())
            .sum()
    }

    fn read_only_compat(&self) -> u32 {
        self.file().superblock().read_only_compat
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.path());
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn version(ordinal: u32) -> String {
    format!("{{\"label\":\"v{ordinal}\",\"schema\":{{\"name\":\"^.+$\"}}}}")
}

/// What a child process does, by role; nothing when it is not a child.
///
/// - `stop`: commit schema version 2 with a failpoint that stops the
///   process, as a crash would.
/// - `fail`: commit schema version 2 with a failpoint that fails it in
///   place, and expect the commit to roll back.
#[test]
fn a_schema_child_process() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let database = Database(PathBuf::from(std::env::var(DATABASE).unwrap()));
    let committed = database.commit(2);
    match role.as_str() {
        "stop" => panic!("the failpoint did not stop the process: {committed:?}"),
        "fail" => {
            assert!(committed.is_err());
            assert_eq!(database.head("items"), Some(1));
            // The child must not remove the database the parent checks.
            std::mem::forget(database);
        }
        other => panic!("unknown child role {other}"),
    }
}

/// Run [`a_schema_child_process`] as `role` with `failpoint` and `action`,
/// and return whether it exited successfully.
fn child(role: &str, database: &Path, failpoint: &str, action: &str) -> bool {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(DATABASE, database)
        .env("FYLO_RUST_FAILPOINT", failpoint)
        .env("FYLO_RUST_FAILPOINT_ACTION", action)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    if !output.status.success() && action != "abort" {
        eprintln!(
            "child {role} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output.status.success()
}

#[test]
fn a_database_commits_schema_versions_and_reopens() {
    let database = Database::create("fxdb-schemas-commit");
    assert_eq!(database.head("items"), None);
    assert_eq!(database.read_only_compat(), 0);
    assert_eq!(database.medium().read_schemas_snapshot().unwrap(), None);
    // Creating collections and writing documents takes no schemas run.
    put_into(&database.writer(), "items", 1).unwrap();
    assert_eq!(database.schemas_extents(), 0);

    database.commit(1).unwrap();
    database.commit(2).unwrap();
    assert_eq!(
        database.commit(2).unwrap_err().code(),
        NativeStorageErrorCode::ConcurrentWrite
    );
    assert_eq!(
        database.commit(4).unwrap_err().code(),
        NativeStorageErrorCode::ConcurrentWrite
    );
    assert_eq!(database.head("items"), Some(2));
    assert_eq!(database.head("plain"), None);
    assert_eq!(
        database.read_only_compat(),
        ContainerSuperblock::READ_ONLY_COMPAT_SCHEMAS
    );
    let run = database
        .checkpoint("items")
        .and_then(|slot| slot.schemas)
        .expect("the checkpoint slot names a schemas run");
    assert!(
        database
            .checkpoint("plain")
            .is_none_or(|slot| slot.schemas.is_none())
    );
    let snapshot = SegmentSchemas::read(&*database.medium())
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.head(), Some(2));
    assert_eq!(snapshot.version(1), Some(version(1).as_bytes()));
    assert_eq!(
        database.medium().schemas_snapshot_stamp().unwrap(),
        Some(SnapshotStamp::Run(run.length, run.sha256))
    );

    // Documents still write, read and index around the records.
    let writer = database.writer();
    for number in 2..40 {
        put_into(&writer, "items", number).unwrap();
    }
    drop(writer);
    database.reopen();

    let root = database.root();
    assert_eq!(root.schema_registry_head("items").unwrap(), Some(2));
    assert_eq!(
        root.read_schema_version("items", 2).unwrap(),
        version(2).as_bytes()
    );
    assert_eq!(
        root.read_schema_version("items", 3).unwrap_err().code(),
        NativeStorageErrorCode::NotFound
    );
    assert_eq!(
        root.collection("items")
            .unwrap()
            .document_ids()
            .unwrap()
            .len(),
        39
    );
    // The location folds since kept the run, in a plain file the same one.
    assert_eq!(
        database.checkpoint("items").and_then(|slot| slot.schemas),
        Some(run)
    );
    assert_eq!(database.schemas_extents(), 1);
    database.commit(3).unwrap();
    assert_eq!(database.head("items"), Some(3));
    assert_eq!(
        database.schemas_extents(),
        1,
        "the replaced run is released"
    );
    let verification = database.writer().verify_segments("items").unwrap();
    assert!(verification.defects.is_empty(), "{verification:?}");
    let report = database.file().verify().unwrap();
    assert!(report.ok, "{report:?}");
}

#[test]
fn a_schemas_snapshot_is_replaced_and_restored() {
    let database = Database::create("fxdb-schemas-replace");
    database.commit(1).unwrap();
    let medium = database.medium();
    let at_one = medium.read_schemas_snapshot().unwrap().unwrap();
    let stamp_at_one = medium.schemas_snapshot_stamp().unwrap();
    database.commit(2).unwrap();
    let at_two = medium.read_schemas_snapshot().unwrap().unwrap();
    assert_ne!(at_one, at_two);
    let stamp_at_two = medium.schemas_snapshot_stamp().unwrap();
    assert!(stamp_at_two.is_some() && stamp_at_two != stamp_at_one);
    // A reader decodes the snapshot once per stamp.
    let root = database.root();
    assert_eq!(root.schema_registry_head("items").unwrap(), Some(2));

    let before = database.checkpoint("items").unwrap();
    medium.restore_schemas_snapshot(Some(&at_one)).unwrap();
    let after = database.checkpoint("items").unwrap();
    assert_eq!(after.checkpoint, before.checkpoint + 1);
    assert_eq!(
        (
            after.locations,
            after.index,
            after.extent_sequence,
            after.offset
        ),
        (
            before.locations,
            before.index,
            before.extent_sequence,
            before.offset
        ),
        "the slot keeps its position and its other runs"
    );
    assert_eq!(medium.read_schemas_snapshot().unwrap().unwrap(), at_one);
    assert_eq!(medium.schemas_snapshot_stamp().unwrap(), stamp_at_one);
    assert_eq!(root.schema_registry_head("items").unwrap(), Some(1));
    assert_eq!(database.schemas_extents(), 1);

    // No snapshot is no run, and so is the empty registry.
    medium.restore_schemas_snapshot(None).unwrap();
    assert_eq!(medium.read_schemas_snapshot().unwrap(), None);
    assert_eq!(medium.schemas_snapshot_stamp().unwrap(), None);
    assert_eq!(database.schemas_extents(), 0);
    let checkpoint = database.checkpoint("items").unwrap().checkpoint;
    medium
        .replace_schemas_snapshot(&SegmentSchemas::empty().encode().unwrap())
        .unwrap();
    assert_eq!(database.checkpoint("items").unwrap().checkpoint, checkpoint);
    assert_eq!(database.schemas_extents(), 0);

    // A writer that loads the registry without a snapshot rebuilds it from
    // the records, as one opening a file written before schemas runs does.
    let rebuilt = SegmentSchemas::load(&*medium, None).unwrap();
    assert_eq!(rebuilt.head(), Some(2));
    assert!(rebuilt.write_if_changed(&*medium).unwrap());
    assert_eq!(medium.read_schemas_snapshot().unwrap().unwrap(), at_two);
    assert_eq!(root.schema_registry_head("items").unwrap(), Some(2));
    database.commit(3).unwrap();
    assert_eq!(database.head("items"), Some(3));
}

#[test]
fn a_damaged_schemas_run_is_rebuilt_from_the_records() {
    let database = Database::create("fxdb-schemas-damaged");
    database.commit(1).unwrap();
    database.commit(2).unwrap();
    let run = database
        .checkpoint("items")
        .and_then(|slot| slot.schemas)
        .unwrap();
    database.reopen();
    let file = ContainerFile::open_unchecked(&database.path()).unwrap();
    file.write_payload(run.first_extent, 2, b"damage").unwrap();
    file.sync().unwrap();
    drop(file);

    let medium = database.medium();
    assert_eq!(
        SegmentSchemas::read(&*medium).unwrap(),
        Some(None),
        "a run that does not check reads as one that does not decode"
    );
    assert_eq!(database.head("items"), Some(2));
    let loaded = SegmentSchemas::load(&*medium, None).unwrap();
    assert_eq!(loaded.head(), Some(2));
    assert!(loaded.write_if_changed(&*medium).unwrap());
    assert!(SegmentSchemas::read(&*medium).unwrap().unwrap().is_some());
    database.commit(3).unwrap();
    assert_eq!(database.head("items"), Some(3));
}

#[test]
fn recovery_finishes_a_schema_commit_a_crash_stopped_after_its_record() {
    for failpoint in ["after-schema-record", "after-schemas-snapshot"] {
        let database = Database::create("fxdb-schemas-crash");
        database.commit(1).unwrap();
        let at_one = database.medium().read_schemas_snapshot().unwrap();
        database.reopen();
        assert!(
            !child("stop", &database.0, failpoint, "abort"),
            "{failpoint}"
        );

        // The record is durable, so recovery lands the new head. Before it,
        // a reader sees the snapshot as the stop left it.
        if failpoint == "after-schema-record" {
            assert_eq!(database.head("items"), Some(1));
            assert_eq!(database.medium().read_schemas_snapshot().unwrap(), at_one);
        }
        let writer = database.writer();
        assert!(writer.recover_collection("items").unwrap(), "{failpoint}");
        assert_eq!(database.head("items"), Some(2), "{failpoint}");
        assert_eq!(
            database.root().read_schema_version("items", 2).unwrap(),
            version(2).as_bytes()
        );
        assert!(!writer.recover_collection("items").unwrap(), "{failpoint}");
        assert!(writer.verify_segments("items").unwrap().defects.is_empty());
        assert_eq!(database.schemas_extents(), 1, "{failpoint}");
        database.commit(3).unwrap();
        put_into(&writer, "items", 1).unwrap();
        drop(writer);
        database.reopen();
        assert_eq!(database.head("items"), Some(3), "{failpoint}");
    }
}

#[test]
fn a_schema_commit_that_fails_in_place_rolls_back() {
    for failpoint in ["after-schema-record", "after-schemas-snapshot"] {
        let database = Database::create("fxdb-schemas-rollback");
        database.commit(1).unwrap();
        let at_one = database.medium().read_schemas_snapshot().unwrap();
        database.reopen();
        assert!(
            child("fail", &database.0, failpoint, "enospc"),
            "{failpoint}"
        );
        assert_eq!(database.head("items"), Some(1), "{failpoint}");
        assert_eq!(database.medium().read_schemas_snapshot().unwrap(), at_one);
        let writer = database.writer();
        assert!(!writer.recover_collection("items").unwrap(), "{failpoint}");
        assert_eq!(database.head("items"), Some(1), "{failpoint}");
        assert_eq!(database.schemas_extents(), 1, "{failpoint}");
        database.commit(2).unwrap();
        assert_eq!(database.head("items"), Some(2), "{failpoint}");
    }
}

/// Compaction is not yet run in a database; the one collection of a
/// collection directory's container shows it carries the schema records
/// and moves the snapshot with them.
#[test]
fn compaction_moves_schema_records_and_the_snapshot_follows() {
    let fixture = Fixture::create("fxdb-schemas-compact");
    let writer = fixture.writer();
    writer
        .commit_schema_version("items", 1, version(1).as_bytes(), |_| Ok(()))
        .unwrap();
    for number in 0..250 {
        put(&writer, number);
    }
    for number in 0..250 {
        patch(&writer, number, 1);
    }
    let medium = fixture.medium();
    let before = SegmentSchemas::read(&*medium).unwrap().unwrap().unwrap();
    let held = before.entries()[0].segment;
    let report = writer.compact_collection("items").unwrap();
    assert!(report.segments_compacted >= 1, "{report:?}");
    let after = SegmentSchemas::read(&*medium).unwrap().unwrap().unwrap();
    assert_eq!(after.head(), Some(1));
    let moved = after.entries()[0].segment;
    assert_ne!(
        moved, held,
        "the record was copied into the compacted segment"
    );
    assert!(medium.segment_numbers().unwrap().contains(&moved));
    assert!(!medium.segment_numbers().unwrap().contains(&held));
    drop(medium);
    fixture.crash();
    let root = NativeRoot::open(&fixture.0).unwrap();
    assert_eq!(root.schema_registry_head("items").unwrap(), Some(1));
    assert_eq!(
        root.read_schema_version("items", 1).unwrap(),
        version(1).as_bytes()
    );
    assert!(writer.verify_segments("items").unwrap().defects.is_empty());
    writer
        .commit_schema_version("items", 2, version(2).as_bytes(), |_| Ok(()))
        .unwrap();
    assert_eq!(root.schema_registry_head("items").unwrap(), Some(2));
    assert_eq!(fixture.bodies()[&identifier(7)]["round"], 1);
}
