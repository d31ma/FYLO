//! Compaction, shrinking and salvage of a `.fxdb` database (ADR 0016
//! sections 6 and 11): every document, schema version and dictionary
//! survives, a reader in another process reads on, a writer process
//! commits during the copy while a second compactor is refused, a shrink
//! keeps other writer processes out, and a crash at each failpoint
//! recovers. The process tests spawn this test binary again, running
//! [`a_compaction_child_process`] in the role an environment variable
//! names.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fylo_format::ContainerExtentKind;
use serde_json::{Value, json};

use super::ContainerMedium;
use super::dictionaries::order;
use crate::write::SegmentCompactor;
use crate::{
    CollectionKind, ContainerFile, ContainerLeaseTiming, ContainerWriteOptions, FilesystemInfo,
    NativeRoot, NativeStorageError, NativeStorageErrorCode, NativeWriteRoot, PutDocumentOptions,
};

const ROLE: &str = "FYLO_FXDB_COMPACTION_CHILD_ROLE";
const DATABASE: &str = "FYLO_FXDB_COMPACTION_CHILD_DATABASE";
const CHILD_TEST: &str =
    "segments::container_medium::tests::database_compaction::a_compaction_child_process";
const WAIT: Duration = Duration::from_mins(2);

/// Documents `items` holds: past 1 MiB of records, so a checkpoint trains
/// a dictionary.
const DOCUMENTS: usize = 520;
/// Documents patched.
const UPDATED: std::ops::Range<usize> = 0..150;
/// Documents deleted.
const DELETED: std::ops::Range<usize> = 150..300;
/// Documents of 16 KiB that does not compress, written and deleted: what
/// a compaction gives back.
const BULK: std::ops::Range<usize> = 1000..1300;
/// Documents `others` holds.
const OTHERS: usize = 40;

fn identifier(number: usize) -> String {
    format!("4VRNF6{:05}", 80_000 + number)
}

/// A batch of four orders as text: about 2 KiB of the kind of document a
/// dictionary is for, in few fields to index.
fn document(number: usize) -> Value {
    let orders: Vec<String> = (4 * number..4 * number + 4)
        .map(|number| order(number).to_string())
        .collect();
    json!({ "orders": orders.join("\n"), "status": "placed" })
}

/// Document `number` as round `round` of patches leaves it.
fn patched(number: usize, round: usize) -> Value {
    let mut body = document(number);
    let fields = body.as_object_mut().unwrap();
    fields.insert("status".into(), json!("returned"));
    fields.insert("round".into(), json!(round));
    body
}

/// What `items` holds for document `number` once [`Database::create`]
/// wrote it, or `None` for a deleted one.
fn expected(number: usize) -> Option<Value> {
    if DELETED.contains(&number) {
        None
    } else if UPDATED.contains(&number) {
        Some(patched(number, 1))
    } else {
        Some(document(number))
    }
}

/// Every document [`Database::create`] left in `items`, by identifier.
fn written() -> BTreeMap<String, Value> {
    (0..DOCUMENTS)
        .filter_map(|number| expected(number).map(|body| (identifier(number), body)))
        .collect()
}

fn other(number: usize) -> Value {
    document(number + 100_000)
}

fn version(ordinal: u32) -> String {
    format!("{{\"label\":\"v{ordinal}\",\"schema\":{{\"status\":\"^[a-z]+$\"}}}}")
}

/// Replace document `number` with its round `round`.
fn patch(writer: &NativeWriteRoot, number: usize, round: usize) {
    writer
        .patch_document(
            "items",
            &identifier(number),
            patched(number, round).to_string().as_bytes(),
            None,
        )
        .unwrap();
}

fn put(
    writer: &NativeWriteRoot,
    collection: &str,
    number: usize,
    body: &Value,
) -> Result<(), NativeStorageError> {
    writer.put_document(
        collection,
        &identifier(number),
        body.to_string().as_bytes(),
        PutDocumentOptions::default(),
    )
}

/// A `.fxdb` database of 1 MiB extents holding `items`, with two schema
/// versions, a dictionary, updates and deletes, and `others`.
struct Database(PathBuf);

impl Database {
    fn create(label: &str) -> Self {
        let database = Self::empty(label);
        let writer = database.writer();
        for ordinal in 1..=2 {
            writer
                .commit_schema_version("items", ordinal, version(ordinal).as_bytes(), |_| Ok(()))
                .unwrap();
        }
        writer
            .commit_schema_version("others", 1, version(1).as_bytes(), |_| Ok(()))
            .unwrap();
        for number in 0..DOCUMENTS {
            put(&writer, "items", number, &document(number)).unwrap();
        }
        writer.rebuild_collection("items").unwrap();
        assert!(
            database.dictionary().is_some(),
            "1 MiB of orders trains a dictionary"
        );
        for number in UPDATED {
            patch(&writer, number, 1);
        }
        for number in BULK {
            let body = json!({
                "padding": fylo_test_support::incompressible_text(number as u64, 16 << 10),
            });
            put(&writer, "items", number, &body).unwrap();
        }
        for number in DELETED.chain(BULK) {
            writer
                .delete_document("items", &identifier(number), None)
                .unwrap();
        }
        for number in 0..OTHERS {
            put(&writer, "others", number, &other(number)).unwrap();
        }
        database.reopen();
        database
    }

    /// A database holding `items` and `others` with no documents.
    fn empty(label: &str) -> Self {
        let directory = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&directory).unwrap();
        let database = Self(std::fs::canonicalize(directory).unwrap());
        ContainerFile::create(&database.path(), ContainerMedium::TEST_EXTENT_SHIFT).unwrap();
        let writer = database.writer();
        for name in ["items", "others"] {
            writer
                .create_collection(name, CollectionKind::Document, None)
                .unwrap();
        }
        database
    }

    /// A copy of this database's file in a directory of its own.
    fn copy(&self, label: &str) -> Self {
        let directory = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&directory).unwrap();
        let copy = Self(std::fs::canonicalize(directory).unwrap());
        std::fs::copy(self.path(), copy.path()).unwrap();
        copy
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

    /// Forget everything this process holds of the database, as a new
    /// process would start.
    fn reopen(&self) {
        NativeRoot::close_database(self.path()).unwrap();
    }

    fn file(&self) -> ContainerFile {
        ContainerFile::open_reader(&self.path()).unwrap()
    }

    fn bytes(&self) -> u64 {
        std::fs::metadata(self.path()).unwrap().len()
    }

    /// The dictionary the newest checkpoint of `items` names, as (extent,
    /// id, length).
    fn dictionary(&self) -> Option<(u64, u32, u32)> {
        let file = self.file();
        let block = file
            .collections()
            .iter()
            .find(|held| held.header.name == "items" && !held.header.dropped)
            .unwrap()
            .block;
        file.read_checkpoint(block)
            .unwrap()
            .and_then(|slot| slot.dictionary)
    }

    fn bodies(root: &NativeRoot, collection: &str) -> BTreeMap<String, Value> {
        let collection = root.collection(collection).unwrap();
        collection
            .document_ids()
            .unwrap()
            .into_iter()
            .map(|id| {
                let bytes = collection.read_document(&id).unwrap().bytes;
                (id, serde_json::from_slice(&bytes).unwrap())
            })
            .collect()
    }

    /// Require that the database holds what [`Self::create`] wrote, and
    /// `more` in `items`, and that its file verifies.
    fn check(&self, more: &[(usize, Value)]) {
        let root = self.root();
        let mut wanted = written();
        wanted.extend(
            more.iter()
                .map(|(number, body)| (identifier(*number), body.clone())),
        );
        let bodies = Self::bodies(&root, "items");
        assert_eq!(bodies.len(), wanted.len());
        assert!(bodies == wanted, "items reads back as written");
        let others: BTreeMap<String, Value> = (0..OTHERS)
            .map(|number| (identifier(number), other(number)))
            .collect();
        assert!(
            Self::bodies(&root, "others") == others,
            "others reads back as written"
        );
        assert_eq!(root.schema_registry_head("items").unwrap(), Some(2));
        assert_eq!(root.schema_registry_head("others").unwrap(), Some(1));
        for ordinal in 1..=2 {
            assert_eq!(
                root.read_schema_version("items", ordinal).unwrap(),
                version(ordinal).as_bytes()
            );
        }
        assert_eq!(
            root.read_schema_version("others", 1).unwrap(),
            version(1).as_bytes()
        );
        let (_, id, _) = self.dictionary().expect("items keeps a dictionary");
        assert!(id >= 1);
        // A crash can leave a dictionary no record names, which the next
        // compaction releases.
        let verified = self.file().verify().unwrap();
        assert!(
            verified.ok
                && verified
                    .problems
                    .iter()
                    .all(|problem| problem.code == "dictionaryUnused"),
            "{verified:?}"
        );
        let writer = self.writer();
        for name in ["items", "others"] {
            let segments = writer.verify_segments(name).unwrap();
            assert!(segments.defects.is_empty(), "{name}: {segments:?}");
        }
    }

    /// Compact until no collection has anything left to compact: 1 MiB
    /// extents compact a few segments at a time.
    fn compact_fully(&self) {
        let writer = self.writer();
        for _ in 0..12 {
            let report = writer.compact_database(false).unwrap();
            if report
                .collections
                .iter()
                .all(|(_, compacted)| compacted.segments_compacted == 0)
            {
                return;
            }
        }
        panic!("compaction never finished");
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.path());
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether byte-range locks are qualified where the database lies: the
/// tests of writer processes side by side need them.
fn locks_qualified(database: &Database) -> bool {
    let file = std::fs::File::open(database.path()).unwrap();
    let filesystem = FilesystemInfo::of(&file).unwrap();
    if !filesystem.byte_range_locks_qualified() {
        eprintln!("skipped: byte-range locks are not qualified on {filesystem:?}");
    }
    filesystem.byte_range_locks_qualified()
}

/// What a child process does, by role; nothing when it is not a child.
///
/// - `read`: read every document of `items` again and again, each read
///   matching what was written, until the parent says `done`.
/// - `write`: write one document to `items` and one to `others` while the
///   parent's compaction copies, sharing the file.
/// - `compact`: compact while the parent's compaction holds `items`, and
///   expect `EBUSY`.
/// - `busy`: write while the parent holds the file to itself for a
///   shrink, and expect `EBUSY`.
/// - `crash` and `crash-shrink`: compact, and shrink, with a failpoint
///   that stops the process.
#[test]
fn a_compaction_child_process() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let path = PathBuf::from(std::env::var(DATABASE).unwrap());
    ContainerMedium::set_write_options(ContainerWriteOptions {
        busy_timeout: Duration::from_millis(300),
        exclusive: false,
        lease_timing: ContainerLeaseTiming::default(),
    });
    match role.as_str() {
        "read" => {
            let done = path.with_file_name("done");
            let wanted = written();
            let mut rounds = 0;
            loop {
                let finished = done.exists();
                let root = NativeRoot::open(&path).unwrap();
                let bodies = Database::bodies(&root, "items");
                assert!(bodies == wanted, "round {rounds} reads what was written");
                rounds += 1;
                std::fs::write(path.with_file_name("read.ready"), b"").unwrap();
                if finished {
                    break;
                }
            }
            eprintln!("{rounds} rounds read");
        }
        "write" => {
            let writer = NativeWriteRoot::open(&path).unwrap();
            put(&writer, "items", 9000, &document(9000)).unwrap();
            put(&writer, "others", 9000, &document(9001)).unwrap();
        }
        "compact" | "busy" => {
            let writer = NativeWriteRoot::open(&path).unwrap();
            let started = Instant::now();
            let error = if role == "compact" {
                writer.compact_database(false).unwrap_err()
            } else {
                put(&writer, "items", 9100, &document(9100)).unwrap_err()
            };
            assert_eq!(error.code(), NativeStorageErrorCode::RootLocked, "{error}");
            assert!(error.to_string().contains("EBUSY"), "{error}");
            assert!(started.elapsed() >= Duration::from_millis(300));
        }
        "crash" | "crash-shrink" => {
            let writer = NativeWriteRoot::open(&path).unwrap();
            let outcome = writer.compact_database(role == "crash-shrink");
            panic!("the failpoint did not stop the process: {outcome:?}");
        }
        other => panic!("unknown child role {other}"),
    }
}

fn spawn(role: &str, database: &Path, environment: &[(&str, &str)]) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(DATABASE, database)
        .envs(environment.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Wait for a child and require that it passed, showing what it printed
/// when it did not.
fn passed(child: Child, role: &str) {
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "child {role} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn compaction_and_a_shrink_keep_every_document_schema_version_and_dictionary() {
    let database = Database::create("fxdb-compact-database");
    let bytes = database.bytes();

    let report = database.writer().compact_database(false).unwrap();
    let names: Vec<&str> = report
        .collections
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(names, ["items", "others"]);
    let (_, items) = &report.collections[0];
    assert!(items.segments_compacted >= 1, "{report:?}");
    assert!(items.bytes_after < items.bytes_before, "{report:?}");
    assert_eq!(report.extents_moved, 0, "only a shrink moves extents");
    database.check(&[]);

    database.compact_fully();
    database.check(&[]);
    assert!(
        database.bytes() >= bytes,
        "without a shrink the file keeps its length or grows"
    );

    let report = database.writer().compact_database(true).unwrap();
    assert!(report.extents_moved >= 1, "{report:?}");
    assert!(report.file_bytes_after < bytes, "{report:?}");
    assert_eq!(database.bytes(), report.file_bytes_after);
    database.check(&[]);

    // A new process reads it all, and writes on.
    database.reopen();
    database.check(&[]);
    put(&database.writer(), "items", 9000, &document(9000)).unwrap();
    database.reopen();
    database.check(&[(9000, document(9000))]);

    // Shrinking again finds nothing to move or cut.
    let again = database.writer().compact_database(true).unwrap();
    assert!(
        again.file_bytes_after <= again.file_bytes_before,
        "{again:?}"
    );
    database.check(&[(9000, document(9000))]);
    let verified = database.file().verify().unwrap();
    assert!(verified.problems.is_empty(), "{verified:?}");
}

#[test]
fn a_reader_process_reads_every_document_while_the_database_compacts_and_shrinks() {
    let database = Database::create("fxdb-compact-reader");
    let mut reader = spawn("read", &database.path(), &[]);
    let ready = database.0.join("read.ready");
    let deadline = Instant::now() + WAIT;
    while !ready.exists() {
        assert!(reader.try_wait().unwrap().is_none(), "the reader stopped");
        assert!(Instant::now() < deadline, "the reader never read");
        std::thread::sleep(Duration::from_millis(10));
    }

    database.compact_fully();
    let report = database.writer().compact_database(true).unwrap();
    assert!(
        report.file_bytes_after < report.file_bytes_before,
        "{report:?}"
    );
    std::fs::write(database.0.join("done"), b"").unwrap();
    passed(reader, "read");
    database.check(&[]);
}

#[test]
fn a_writer_process_commits_during_the_copy_and_a_second_compactor_is_refused() {
    let database = Database::create("fxdb-compact-writers");
    if !locks_qualified(&database) {
        return;
    }
    let path = database.path();
    let paused = Arc::new(AtomicBool::new(false));
    let hook: crate::write::CopiedHook = {
        let (path, paused) = (path.clone(), Arc::clone(&paused));
        Arc::new(move |collection: &Path| {
            if collection.starts_with(&path) && !paused.swap(true, Ordering::SeqCst) {
                // The copy runs holding the compactor byte, not the writer
                // byte.
                passed(spawn("write", &path, &[]), "write");
                passed(spawn("compact", &path, &[]), "compact");
            }
        })
    };
    SegmentCompactor::set_copied_hook(Some(hook));
    let compacted = database.writer().compact_database(false);
    SegmentCompactor::set_copied_hook(None);
    let report = compacted.unwrap();
    assert!(paused.load(Ordering::SeqCst), "the hook ran");
    assert!(
        report.collections[0].1.segments_compacted >= 1,
        "{report:?}"
    );

    let root = database.root();
    let items = root.collection("items").unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&items.read_document(&identifier(9000)).unwrap().bytes)
            .unwrap(),
        document(9000)
    );
    let others = root.collection("others").unwrap();
    assert_eq!(others.document_ids().unwrap().len(), OTHERS + 1);
    database.reopen();
    database.compact_fully();
    database.reopen();
    let bodies = Database::bodies(&database.root(), "items");
    assert_eq!(bodies.get(&identifier(9000)), Some(&document(9000)));
    let verified = database.file().verify().unwrap();
    assert!(verified.ok, "{verified:?}");
}

#[test]
fn a_shrink_holds_the_file_and_another_writer_process_is_busy() {
    let database = Database::empty("fxdb-compact-exclusive");
    if !locks_qualified(&database) {
        return;
    }
    let root = database.root();
    let held = root.database().unwrap().exclusively(|| {
        passed(spawn("busy", &database.path(), &[]), "busy");
        Ok(())
    });
    held.unwrap();
    // Shared again, another writer process writes.
    passed(spawn("write", &database.path(), &[]), "write");
    database.reopen();
    let bodies = Database::bodies(&database.root(), "items");
    assert_eq!(bodies.get(&identifier(9000)), Some(&document(9000)));
    assert!(!bodies.contains_key(&identifier(9100)));
}

#[test]
fn a_crash_at_each_compaction_and_shrink_failpoint_recovers() {
    let prepared = Database::create("fxdb-compact-crash");
    // Compact once, so a shrink has free extents to move into and cut.
    prepared.compact_fully();
    prepared.reopen();
    let cases = [
        ("crash", "after-compaction-copy"),
        ("crash", "after-compaction-sync"),
        ("crash", "after-compaction-record"),
        ("crash", "after-compaction-remove"),
        ("crash-shrink", "after-shrink-relocate"),
        ("crash-shrink", "after-shrink-truncate"),
    ];
    let more: Vec<(usize, Value)> = (9000..9040)
        .map(|number| (number, patched(number, 2)))
        .collect();
    for (role, failpoint) in cases {
        let database = prepared.copy("fxdb-compact-crash-case");
        // Dead entries for the compaction to drop.
        let writer = database.writer();
        for number in 9000..9040 {
            put(&writer, "items", number, &document(number)).unwrap();
            patch(&writer, number, 2);
        }
        drop(writer);
        database.reopen();
        let output = spawn(
            role,
            &database.path(),
            &[
                ("FYLO_RUST_FAILPOINT", failpoint),
                ("FYLO_RUST_FAILPOINT_ACTION", "abort"),
            ],
        )
        .wait_with_output()
        .unwrap();
        let printed = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success() && !printed.contains("did not stop"),
            "child {role} at {failpoint}:\n{printed}"
        );

        database.check(&more);
        let report = database.writer().compact_database(true).unwrap();
        assert!(
            report.file_bytes_after <= report.file_bytes_before,
            "{failpoint}: {report:?}"
        );
        database.check(&more);
        database.reopen();
        database.check(&more);
    }
}

#[test]
fn a_database_salvages_to_a_directory_root_stepping_over_a_damaged_extent() {
    let database = Database::create("fxdb-salvage-database");
    let out = database.0.join("salvaged");
    let report = database.writer().salvage_database(&out).unwrap();
    let names: Vec<&str> = report
        .collections
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(names, ["items", "others"]);
    assert!(
        report
            .collections
            .iter()
            .all(|(_, salvaged)| salvaged.damaged.is_empty())
    );
    assert_eq!(report.collections[0].1.schema_versions, [1, 2]);
    assert!(
        !NativeRoot::names_database(&out),
        "a salvage writes a directory root"
    );
    let salvaged = NativeRoot::open(&out).unwrap();
    let wanted = written();
    assert!(
        Database::bodies(&salvaged, "items") == wanted,
        "the salvage holds every live document"
    );
    assert_eq!(Database::bodies(&salvaged, "others").len(), OTHERS);
    assert_eq!(salvaged.schema_registry_head("items").unwrap(), Some(2));
    assert_eq!(
        salvaged.read_schema_version("items", 2).unwrap(),
        version(2).as_bytes()
    );
    assert_eq!(
        database.writer().salvage_database(&out).unwrap_err().code(),
        NativeStorageErrorCode::Unsupported,
        "the output must be empty"
    );

    // Damage the header of the first segment extent of `items`: the
    // salvage steps over it and keeps the rest.
    database.reopen();
    let extent = {
        let file = database.file();
        let id = file
            .collections()
            .iter()
            .find(|held| held.header.name == "items")
            .unwrap()
            .header
            .id;
        file.allocator()
            .owned(ContainerExtentKind::Segment, id)
            .first()
            .map(|(index, _)| *index)
            .unwrap()
    };
    damage(&database.path(), extent);
    let out = database.0.join("salvaged-damaged");
    let report = database.writer().salvage_database(&out).unwrap();
    let (_, items) = &report.collections[0];
    assert_eq!(items.damaged.len(), 1, "{items:?}");
    assert!(items.recovered > 0 && !items.lost.is_empty(), "{items:?}");
    let salvaged = NativeRoot::open(&out).unwrap();
    let bodies = Database::bodies(&salvaged, "items");
    assert!(
        bodies.iter().all(|(id, body)| wanted.get(id) == Some(body)),
        "what is salvaged is what was written"
    );
    assert_eq!(Database::bodies(&salvaged, "others").len(), OTHERS);
}

/// Overwrite the first bytes of `extent`, its header.
fn damage(path: &Path, extent: u64) {
    use std::io::{Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(
        extent * (1 << ContainerMedium::TEST_EXTENT_SHIFT),
    ))
    .unwrap();
    file.write_all(&[0xA5; 64]).unwrap();
    file.sync_all().unwrap();
}
