//! A write a `.fxdb` database's writer left unfinished, as its reader finds
//! it (ADR 0016 section 7), and a checkpoint that fails in place. A reader
//! finishes the write of a writer process that stopped, leaves that of a
//! running one alone, and reads past one it cannot finish without becoming
//! the file's single writer; a checkpoint that fails on a full disk leaves
//! nothing for recovery. The process tests spawn this test binary again,
//! running [`a_stranded_write_child_process`] in the role an environment
//! variable names.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fylo_format::ContainerGenerationSlot;
use serde_json::json;

use super::{ContainerMedium, identifier};
use crate::{
    ByteRangeLockFile, CollectionKind, ContainerFile, ContainerLease, ContainerLeaseTiming,
    ContainerWriteOptions, FilesystemInfo, GenerationStatus, LockByte, LockMode, NativeRoot,
    NativeWriteRoot, PutDocumentOptions,
};

const ROLE: &str = "FYLO_FXDB_STRANDED_CHILD_ROLE";
const DATABASE: &str = "FYLO_FXDB_STRANDED_CHILD_DATABASE";
const EXCLUSIVE: &str = "FYLO_FXDB_STRANDED_CHILD_EXCLUSIVE";
const CHILD_TEST: &str =
    "segments::container_medium::tests::stranded_writes::a_stranded_write_child_process";
const WAIT: Duration = Duration::from_mins(1);
/// Documents a database starts with.
const DOCUMENTS: usize = 6;
/// The document a crashed writer was writing.
const CRASHED: usize = 99;

/// What a child process does, by role; nothing when it is not a child.
///
/// - `crash`: write one more document to `items`, in exclusive mode (the
///   lease) when asked, with a failpoint that stops the process once the
///   record is appended and the slot says `writing`.
/// - `hold`: hold the lease byte shared and the writer byte of `items`, as
///   a writer process does for the length of a write, say so, and keep
///   them until the parent says `release`.
/// - `compact`: compact the database with a failpoint that fails the
///   checkpoint on a full disk, and expect the error.
#[test]
fn a_stranded_write_child_process() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let path = PathBuf::from(std::env::var(DATABASE).unwrap());
    ContainerMedium::set_write_options(ContainerWriteOptions {
        busy_timeout: Duration::from_millis(300),
        exclusive: std::env::var(EXCLUSIVE).is_ok(),
        lease_timing: ContainerLeaseTiming::default(),
    });
    match role.as_str() {
        "crash" => {
            let outcome = put(&NativeWriteRoot::open(&path).unwrap(), CRASHED);
            panic!("the failpoint did not stop the process: {outcome:?}");
        }
        "hold" => {
            let handle = ByteRangeLockFile::open(&path).unwrap();
            let _lease = handle
                .lock(LockByte::LEASE, LockMode::Shared, WAIT)
                .unwrap();
            let _writer = handle
                .try_lock(
                    LockByte::collection_writer(block(&path)),
                    LockMode::Exclusive,
                )
                .unwrap()
                .expect("nothing else writes items");
            std::fs::write(path.with_file_name("hold.ready"), b"").unwrap();
            let release = path.with_file_name("release");
            let deadline = Instant::now() + WAIT;
            while !release.exists() {
                assert!(Instant::now() < deadline, "never released");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        "compact" => {
            let error = NativeWriteRoot::open(&path)
                .unwrap()
                .compact_database(false)
                .unwrap_err();
            assert!(error.to_string().contains("space"), "{error}");
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

/// Run a `crash` child, in exclusive mode when asked, and require that it
/// stopped at the failpoint, leaving `items` published as `writing`.
fn crash(database: &Database, exclusive: bool) {
    let mut environment = vec![
        ("FYLO_RUST_FAILPOINT", "after-log-append"),
        ("FYLO_RUST_FAILPOINT_ACTION", "abort"),
    ];
    if exclusive {
        environment.push((EXCLUSIVE, "1"));
    }
    let output = spawn("crash", &database.path(), &environment)
        .wait_with_output()
        .unwrap();
    let printed = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && !printed.contains("did not stop"),
        "child crash:\n{printed}"
    );
    assert!(database.slot().writing, "the crash left `items` writing");
}

fn put(writer: &NativeWriteRoot, number: usize) -> Result<(), crate::NativeStorageError> {
    writer.put_document(
        "items",
        &identifier(number),
        json!({ "number": number }).to_string().as_bytes(),
        PutDocumentOptions::default(),
    )
}

/// The directory block of `items` in the database at `path`.
fn block(path: &Path) -> u32 {
    ContainerFile::open_reader(path)
        .unwrap()
        .collections()
        .iter()
        .find(|held| held.header.name == "items" && !held.header.dropped)
        .unwrap()
        .block
}

/// A `.fxdb` database of 1 MiB extents holding `items`, with documents
/// written, patched and deleted, so a compaction has dead entries to drop.
struct Database(PathBuf);

impl Database {
    fn create(label: &str) -> Self {
        let directory = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&directory).unwrap();
        let database = Self(std::fs::canonicalize(directory).unwrap());
        ContainerFile::create(&database.path(), ContainerMedium::TEST_EXTENT_SHIFT).unwrap();
        let writer = NativeWriteRoot::open(database.path()).unwrap();
        writer
            .create_collection("items", CollectionKind::Document, None)
            .unwrap();
        for number in 0..DOCUMENTS + 2 {
            put(&writer, number).unwrap();
        }
        for number in 0..DOCUMENTS {
            writer
                .patch_document("items", &identifier(number), br#"{"patched":true}"#, None)
                .unwrap();
        }
        for number in DOCUMENTS..DOCUMENTS + 2 {
            writer
                .delete_document("items", &identifier(number), None)
                .unwrap();
        }
        drop(writer);
        database.reopen();
        database
    }

    fn path(&self) -> PathBuf {
        self.0.join("db.fxdb")
    }

    /// Forget everything this process holds of the database, as a new
    /// process would start.
    fn reopen(&self) {
        NativeRoot::close_database(self.path()).unwrap();
    }

    /// The generation slot of `items`, as the file holds it.
    fn slot(&self) -> ContainerGenerationSlot {
        let path = self.path();
        ContainerFile::open_reader(&path)
            .unwrap()
            .read_generation(block(&path))
            .unwrap()
            .unwrap()
    }

    /// The identifiers of the documents `items` holds, each read back.
    fn documents(&self) -> Vec<String> {
        let collection = NativeRoot::open(self.path())
            .unwrap()
            .collection("items")
            .unwrap();
        let ids = collection.document_ids().unwrap();
        for id in &ids {
            assert!(!collection.read_document(id).unwrap().bytes.is_empty());
        }
        ids
    }

    fn locks_qualified(&self) -> bool {
        let file = std::fs::File::open(self.path()).unwrap();
        let filesystem = FilesystemInfo::of(&file).unwrap();
        if !filesystem.byte_range_locks_qualified() {
            eprintln!("skipped: byte-range locks are not qualified on {filesystem:?}");
        }
        filesystem.byte_range_locks_qualified()
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.path());
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn written() -> Vec<String> {
    (0..DOCUMENTS).map(identifier).collect()
}

#[test]
fn a_reader_finishes_the_write_of_a_writer_process_that_stopped() {
    let database = Database::create("fxdb-stranded-finish");
    if !database.locks_qualified() {
        return;
    }
    crash(&database, false);

    let collection = NativeRoot::open(database.path())
        .unwrap()
        .collection("items")
        .unwrap();
    let state = collection.generation().unwrap();
    assert_eq!(state.state, GenerationStatus::Stable);
    assert!(!database.slot().writing, "the reader finished the write");
    // The appended record was whole, so recovery keeps it.
    let mut wanted = written();
    wanted.push(identifier(CRASHED));
    assert_eq!(database.documents(), wanted);
    database.reopen();
    let recovered = NativeWriteRoot::open(database.path())
        .unwrap()
        .recover_collection("items")
        .unwrap();
    assert!(!recovered, "nothing was left for recovery");
}

#[test]
fn a_reader_leaves_the_write_of_a_running_writer_process_alone() {
    let database = Database::create("fxdb-stranded-running");
    if !database.locks_qualified() {
        return;
    }
    crash(&database, false);
    // Another process holds the collection as a writer does mid-write.
    let mut holder = spawn("hold", &database.path(), &[]);
    let ready = database.0.join("hold.ready");
    let deadline = Instant::now() + WAIT;
    while !ready.exists() {
        assert!(holder.try_wait().unwrap().is_none(), "the holder stopped");
        assert!(Instant::now() < deadline, "the holder never held items");
        std::thread::sleep(Duration::from_millis(10));
    }

    let collection = NativeRoot::open(database.path())
        .unwrap()
        .collection("items")
        .unwrap();
    let slot = database.slot();
    let state = collection.generation().unwrap();
    assert_eq!(state.state, GenerationStatus::Writing);
    assert_eq!(state.generation, slot.generation);
    assert!(
        database.slot().writing,
        "a running writer's write is its own"
    );

    std::fs::write(database.0.join("release"), b"").unwrap();
    passed(holder, "hold");
    let state = collection.generation().unwrap();
    assert_eq!(state.state, GenerationStatus::Stable);
    assert!(
        !database.slot().writing,
        "the reader finished it once let go"
    );
}

#[test]
fn a_reader_reads_past_a_write_left_under_the_lease() {
    let database = Database::create("fxdb-stranded-lease");
    crash(&database, true);
    assert!(
        ContainerLease::held_generation(&database.path())
            .unwrap()
            .is_some(),
        "the crashed writer left its lease held"
    );

    let slot = database.slot();
    let collection = NativeRoot::open(database.path())
        .unwrap()
        .collection("items")
        .unwrap();
    let state = collection.generation().unwrap();
    assert_eq!(state.state, GenerationStatus::Stable);
    assert_eq!(state.generation, slot.generation);
    // Finishing it would take the lease over, waiting for its takeover
    // time, and keep every other writer out after: the reader reads the
    // collection as the slot last published it instead.
    assert_eq!(database.documents(), written());
    assert!(
        database.slot().writing,
        "the write is left to the next writer"
    );
}

#[test]
fn a_checkpoint_that_fails_in_place_rolls_back_before_it_returns() {
    let database = Database::create("fxdb-stranded-checkpoint");
    passed(
        spawn(
            "compact",
            &database.path(),
            &[
                ("FYLO_RUST_FAILPOINT", "after-checkpoint-flush"),
                ("FYLO_RUST_FAILPOINT_ACTION", "enospc"),
            ],
        ),
        "compact",
    );
    assert!(
        !database.slot().writing,
        "the failed checkpoint published stable again"
    );
    let writer = NativeWriteRoot::open(database.path()).unwrap();
    assert!(
        !writer.recover_collection("items").unwrap(),
        "nothing was left for recovery"
    );
    assert_eq!(database.documents(), written());
    writer.compact_database(false).unwrap();
    assert_eq!(database.documents(), written());
    drop(writer);
    database.reopen();
    assert_eq!(database.documents(), written());
    let verified = ContainerFile::open_reader(&database.path())
        .unwrap()
        .verify()
        .unwrap();
    assert!(verified.ok, "{verified:?}");
}
