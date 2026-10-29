//! A write, compaction or shrink of a `.vdbx` database that meets an I/O
//! failure while it takes an extent or checkpoints the map, as a full disk
//! or a filesystem that cannot punch a hole gives: it fails alone and
//! leaves the file and the open as they were, so the next write works,
//! nothing is left for recovery, the file verifies and a reopen reads
//! every document. Each failure is made once, on the test's own thread
//! ([`fault`]).

use std::collections::BTreeMap;
use std::path::PathBuf;

use fylo_format::ContainerExtentKind;
use serde_json::{Value, json};

use super::{ContainerMedium, identifier};
use crate::container_file::fault::{self, ContainerFault};
use crate::{
    CollectionKind, ContainerFile, NativeRoot, NativeStorageError, NativeWriteRoot,
    PutDocumentOptions,
};

/// Characters of incompressible text in a document, so a 1 MiB segment
/// extent fills after ten of them.
const PADDING: usize = 128 << 10;
/// Documents `others` holds.
const OTHERS: usize = 3;
/// Writes that reach the failure at the latest: past several new segments
/// and map checkpoints.
const REACH: usize = 60;

fn body(number: usize) -> Value {
    json!({
        "number": number,
        "padding": fylo_test_support::incompressible_text(number as u64, PADDING),
    })
}

/// A `.vdbx` database of 1 MiB extents holding `items` and `others`, and
/// the documents written to `items`.
struct Database {
    directory: PathBuf,
    items: BTreeMap<String, Value>,
    next: usize,
}

impl Database {
    fn create(label: &str) -> Self {
        let directory = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&directory).unwrap();
        let database = Self {
            directory: std::fs::canonicalize(directory).unwrap(),
            items: BTreeMap::new(),
            next: 0,
        };
        ContainerFile::create(&database.path(), ContainerMedium::TEST_EXTENT_SHIFT).unwrap();
        let writer = database.writer();
        for name in ["items", "others"] {
            writer
                .create_collection(name, CollectionKind::Document, None)
                .unwrap();
        }
        for number in 0..OTHERS {
            writer
                .put_document(
                    "others",
                    &identifier(number),
                    json!({ "other": number }).to_string().as_bytes(),
                    PutDocumentOptions::default(),
                )
                .unwrap();
        }
        database
    }

    /// A copy of the database, closed first, in a directory of its own.
    fn copy(&self, label: &str) -> Self {
        NativeRoot::close_database(self.path()).unwrap();
        let directory = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&directory).unwrap();
        let copy = Self {
            directory: std::fs::canonicalize(directory).unwrap(),
            items: self.items.clone(),
            next: self.next,
        };
        std::fs::copy(self.path(), copy.path()).unwrap();
        copy
    }

    /// A database whose segments a compaction has given back, with most
    /// of what it still holds dead, so a compaction has more to copy and
    /// a shrink has extents to move and cut.
    fn compacted(label: &str) -> Self {
        let mut database = Self::create(label);
        let writer = database.writer();
        database.put_all(&writer, 20);
        database.delete_most(&writer);
        Self::compact(&writer);
        database
    }

    fn path(&self) -> PathBuf {
        self.directory.join("db.vdbx")
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(self.path()).unwrap()
    }

    /// Write the next document to `items`.
    fn put(&mut self, writer: &NativeWriteRoot) -> Result<(), NativeStorageError> {
        let (id, body) = (identifier(self.next), body(self.next));
        self.next += 1;
        writer.put_document(
            "items",
            &id,
            body.to_string().as_bytes(),
            PutDocumentOptions::default(),
        )?;
        self.items.insert(id, body);
        Ok(())
    }

    fn put_all(&mut self, writer: &NativeWriteRoot, count: usize) {
        for _ in 0..count {
            self.put(writer).unwrap();
        }
    }

    /// Delete every document of `items` but one in eight, so a compaction
    /// gives most segment extents back.
    fn delete_most(&mut self, writer: &NativeWriteRoot) {
        let doomed: Vec<String> = self.items.keys().skip(1).step_by(8).cloned().collect();
        let kept: Vec<String> = self
            .items
            .keys()
            .filter(|id| !doomed.contains(id))
            .cloned()
            .collect();
        for id in kept {
            writer.delete_document("items", &id, None).unwrap();
            self.items.remove(&id);
        }
    }

    /// Arm `fault` to fail once `passes` of its writes have passed, and
    /// write until a write meets it, then require that the failed write
    /// left its document whole or absent, and that the writes after it
    /// work.
    fn fail_a_write(&mut self, writer: &NativeWriteRoot, (fault, passes): (ContainerFault, usize)) {
        fault::arm_after(fault, passes);
        for _ in 0..REACH {
            let number = self.next;
            if self.put(writer).is_ok() {
                continue;
            }
            assert!(!fault::pending(), "only the armed failure fails a write");
            // A failure after the commit, as a checkpoint's, still leaves
            // the document committed.
            let id = identifier(number);
            if let Ok(stored) = NativeRoot::open(self.path())
                .unwrap()
                .collection("items")
                .unwrap()
                .read_document(&id)
            {
                assert_eq!(
                    serde_json::from_slice::<Value>(&stored.bytes).unwrap(),
                    body(number)
                );
                self.items.insert(id, body(number));
            }
            self.put_all(writer, 12);
            return;
        }
        panic!("no write met {fault:?}");
    }

    /// Require that nothing is left for recovery, that the file and the
    /// segments verify, and that every document reads back, before and
    /// after a reopen, which writes on.
    fn check(&mut self) {
        let writer = self.writer();
        for name in ["items", "others"] {
            assert!(
                !writer.recover_collection(name).unwrap(),
                "{name}: nothing was left for recovery"
            );
            let segments = writer.verify_segments(name).unwrap();
            assert!(segments.defects.is_empty(), "{name}: {segments:?}");
        }
        self.check_documents();
        let verified = ContainerFile::open_reader(&self.path())
            .unwrap()
            .verify()
            .unwrap();
        // A checkpoint that fails after training a dictionary leaves one
        // no record names, as a stop there would, which the next
        // compaction releases.
        assert!(
            verified.ok
                && verified
                    .problems
                    .iter()
                    .all(|problem| problem.code == "dictionaryUnused"),
            "{verified:?}"
        );
        drop(writer);
        NativeRoot::close_database(self.path()).unwrap();
        self.check_documents();
        self.put_all(&self.writer(), 2);
        self.check_documents();
    }

    fn check_documents(&self) {
        let root = NativeRoot::open(self.path()).unwrap();
        let items = root.collection("items").unwrap();
        let mut ids = items.document_ids().unwrap();
        ids.sort();
        assert_eq!(ids, self.items.keys().cloned().collect::<Vec<_>>());
        for (id, body) in &self.items {
            let stored = items.read_document(id).unwrap();
            assert!(
                serde_json::from_slice::<Value>(&stored.bytes).unwrap() == *body,
                "{id} reads back as written"
            );
        }
        let others = root.collection("others").unwrap();
        assert_eq!(others.document_ids().unwrap().len(), OTHERS);
    }

    /// Compact the database once: enough to give segment extents back.
    fn compact(writer: &NativeWriteRoot) {
        let report = writer.compact_database(false).unwrap();
        assert!(
            report
                .collections
                .iter()
                .any(|(_, compacted)| compacted.segments_compacted > 0),
            "{report:?}"
        );
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.path());
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// The failures taking an extent or checkpointing the map can meet.
const FAULTS: [ContainerFault; 6] = [
    ContainerFault::Grow,
    ContainerFault::ClearExtent,
    ContainerFault::ExtentHeader(ContainerExtentKind::Segment),
    ContainerFault::ExtentHeader(ContainerExtentKind::Map),
    ContainerFault::Map,
    ContainerFault::Superblock,
];

/// The failures taking a free extent or checkpointing the map can meet.
const REUSE_FAULTS: [ContainerFault; 4] = [
    ContainerFault::ClearExtent,
    ContainerFault::ExtentHeader(ContainerExtentKind::Segment),
    ContainerFault::Map,
    ContainerFault::Superblock,
];

#[test]
fn a_collection_created_on_a_failing_extent_is_completed_by_creating_it_again() {
    for fault in REUSE_FAULTS {
        let mut database = Database::create("vdbx-fault-create");
        let writer = database.writer();
        fault::arm(fault);
        assert!(
            writer
                .create_collection("late", CollectionKind::Document, None)
                .is_err(),
            "{fault:?} fails the creation"
        );
        assert!(!fault::pending(), "{fault:?} was met");
        database.put_all(&writer, 2);
        assert!(
            !writer
                .create_collection("late", CollectionKind::Document, None)
                .unwrap(),
            "created again, the collection is completed rather than added"
        );
        writer
            .put_document(
                "late",
                &identifier(9999),
                b"{\"late\":true}",
                PutDocumentOptions::default(),
            )
            .unwrap();
        drop(writer);
        database.check();
        assert_eq!(
            NativeRoot::open(database.path())
                .unwrap()
                .collection("late")
                .unwrap()
                .document_ids()
                .unwrap(),
            vec![identifier(9999)]
        );
    }
}

/// Each failure at the first of its writes, and a superblock write at
/// the next few too: a write whose checkpoint folds the locations and
/// seals the segment checkpoints the map several times, the last of them
/// taking the next segment.
#[test]
fn a_write_that_fails_taking_an_extent_or_checkpointing_fails_alone() {
    let later = (1..=3).map(|passes| (ContainerFault::Superblock, passes));
    for (fault, passes) in FAULTS.into_iter().map(|fault| (fault, 0)).chain(later) {
        let mut database = Database::create("vdbx-fault-write");
        let writer = database.writer();
        database.put_all(&writer, 4);
        database.fail_a_write(&writer, (fault, passes));
        drop(writer);
        database.check();
    }
}

#[test]
fn a_write_that_fails_reusing_a_compacted_extent_fails_alone() {
    let compacted = Database::compacted("vdbx-fault-reuse");
    for fault in REUSE_FAULTS {
        let mut database = compacted.copy("vdbx-fault-reuse");
        let writer = database.writer();
        database.fail_a_write(&writer, (fault, 0));
        drop(writer);
        database.check();
    }
}

/// Each failure at each of its writes in a compaction and shrink, until
/// one runs through without meeting it: taking the copy's extent, the
/// checkpoints that adopt it and give the old segments back, the moves of
/// the runs and the map, and the cut of the free tail.
#[test]
fn a_compaction_or_shrink_that_fails_taking_an_extent_or_checkpointing_fails_alone() {
    let compacted = Database::compacted("vdbx-fault-shrink");
    for fault in [
        ContainerFault::ClearExtent,
        ContainerFault::ExtentHeader(ContainerExtentKind::Segment),
        ContainerFault::Superblock,
    ] {
        let mut passes = 0;
        loop {
            let mut database = compacted.copy("vdbx-fault-shrink");
            let writer = database.writer();
            fault::arm_after(fault, passes);
            if writer.compact_database(true).is_ok() {
                assert!(fault::pending(), "a shrink that meets {fault:?} fails");
                assert!(passes > 0, "the shrink meets {fault:?}");
                break;
            }
            assert!(!fault::pending(), "{fault:?} fails the shrink");
            database.put_all(&writer, 2);
            writer.compact_database(true).unwrap();
            database.put_all(&writer, 2);
            drop(writer);
            database.check();
            passes += 1;
        }
    }
}
