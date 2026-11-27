//! A `.vdbx` file opened as a BYCKR root (ADR 0016 section 11,
//! `docs/vdbx-segment-backend.md`, "Root integration"): every collection
//! lives in the one file, and nothing is created beside it but the lease
//! file a writer holds.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use byckr_query::{IndexLookupValue, QueryLimits, ScanQuery, index_entries_for_document};
use byckr_storage_native::{
    ByteRangeLockFile, CollectionKind, CollectionLayout, ContainerFile, FilesystemInfo, LockByte,
    LockMode, NativeQueue, NativeRoot, NativeStorageErrorCode, NativeWriteRoot, PutDocumentOptions,
    RootLease, TURN_LEVELS,
};
use serde_json::{Map, Value, json};

struct Fixture {
    directory: PathBuf,
}

impl Fixture {
    fn create(label: &str) -> Self {
        let directory = byckr_test_support::TestFolder::path().join(format!(
            "fylo-vdbx-root-{label}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            {
                static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        fs::create_dir(&directory).unwrap();
        let fixture = Self {
            directory: fs::canonicalize(directory).unwrap(),
        };
        NativeRoot::create_database(fixture.database()).unwrap();
        fixture
    }

    fn database(&self) -> PathBuf {
        self.directory.join("db.vdbx")
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(self.database()).unwrap()
    }

    fn root(&self) -> NativeRoot {
        NativeRoot::open(self.database()).unwrap()
    }

    /// Forget everything this process holds of the database, as a new
    /// process would start.
    fn reopen(&self) {
        NativeRoot::close_database(self.database()).unwrap();
    }

    /// The directory holds the database and at most its lease file.
    fn assert_only_the_database(&self) {
        let names: Vec<String> = fs::read_dir(&self.directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"db.vdbx".to_owned()), "{names:?}");
        let others: Vec<&String> = names
            .iter()
            .filter(|name| name.as_str() != "db.vdbx")
            .collect();
        assert!(
            others.len() <= 1 && others.iter().all(|name| name.starts_with("db.vdbx.lease.")),
            "unexpected entries beside the database: {names:?}"
        );
    }
}

/// The child processes of one test. A child still running when the test
/// fails is stopped, so a failed test leaves no orphan behind (a reader
/// waits for a `done` file that a failed test never writes).
struct Children(Vec<(String, Option<Child>)>);

impl Children {
    fn named(&mut self) -> impl Iterator<Item = (&String, &mut Child)> {
        self.0
            .iter_mut()
            .filter_map(|(name, child)| child.as_mut().map(|child| (&*name, child)))
    }

    /// Take the child at `index` out of the group to wait for it.
    fn take(&mut self, index: usize) -> (String, Child) {
        let (name, child) = &mut self.0[index];
        (name.clone(), child.take().unwrap())
    }
}

impl Drop for Children {
    fn drop(&mut self) {
        for child in self.0.iter_mut().filter_map(|(_, child)| child.as_mut()) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.database());
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 20_000 + number)
}

fn body(collection: &str, number: usize) -> Value {
    json!({ "group": number % 3, "name": format!("{collection}{number}") })
}

fn put(writer: &NativeWriteRoot, collection: &str, number: usize) {
    writer
        .put_document(
            collection,
            &identifier(number),
            body(collection, number).to_string().as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn read(root: &NativeRoot, collection: &str, number: usize) -> Value {
    let bytes = root
        .collection(collection)
        .unwrap()
        .read_document(&identifier(number))
        .unwrap()
        .bytes;
    serde_json::from_slice(&bytes).unwrap()
}

/// The index keys of every live document of `collection`, as a rebuild
/// from the documents would write them.
fn expected_keys(root: &NativeRoot, collection: &str) -> BTreeSet<String> {
    let opened = root.collection(collection).unwrap();
    let mut keys = BTreeSet::new();
    for id in opened.document_ids().unwrap() {
        let bytes = opened.read_document(&id).unwrap().bytes;
        let fields: Map<String, Value> = serde_json::from_slice(&bytes).unwrap();
        keys.extend(
            index_entries_for_document(&id, &fields, |_, value| {
                Ok::<_, std::convert::Infallible>(IndexLookupValue::plain(value))
            })
            .unwrap(),
        );
    }
    keys
}

fn index_keys(root: &NativeRoot, collection: &str) -> BTreeSet<String> {
    let snapshot = root
        .collection(collection)
        .unwrap()
        .index_snapshot()
        .unwrap();
    String::from_utf8_lossy(snapshot.as_bytes())
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The identifiers the prefix index finds under the key of `field` in
/// document `number`.
fn find_by_index(root: &NativeRoot, collection: &str, field: &str, number: usize) -> Vec<String> {
    let id = identifier(number);
    let fields = body(collection, number);
    let key = index_entries_for_document(&id, fields.as_object().unwrap(), |_, value| {
        Ok::<_, std::convert::Infallible>(IndexLookupValue::plain(value))
    })
    .unwrap()
    .into_iter()
    .find(|key| key.starts_with(&format!("{field}/")))
    .unwrap();
    let prefix = key.strip_suffix(&id).unwrap().to_owned();
    let snapshot = root
        .collection(collection)
        .unwrap()
        .index_snapshot()
        .unwrap();
    let mut found: Vec<String> = snapshot
        .scan(
            &[ScanQuery {
                prefix,
                range: None,
            }],
            QueryLimits::default(),
        )
        .unwrap()
        .into_iter()
        .map(|id| String::from_utf8(id).unwrap())
        .collect();
    found.sort();
    found
}

fn code_of<T: std::fmt::Debug>(
    result: Result<T, byckr_storage_native::NativeStorageError>,
) -> NativeStorageErrorCode {
    result.unwrap_err().code()
}

#[test]
fn a_database_keeps_two_collections_in_one_file_across_a_reopen() {
    let fixture = Fixture::create("roundtrip");
    let writer = fixture.writer();
    assert!(
        writer
            .create_collection("users", CollectionKind::Document, None)
            .unwrap()
    );
    assert!(
        writer
            .create_collection("orders", CollectionKind::Document, None)
            .unwrap()
    );
    // Creating again completes nothing and reports the collection existed.
    assert!(
        !writer
            .create_collection("users", CollectionKind::Document, None)
            .unwrap()
    );
    for number in 0..12 {
        put(&writer, "users", number);
    }
    for number in 0..5 {
        put(&writer, "orders", number);
    }
    writer
        .patch_document("users", &identifier(1), br#"{"name":"renamed"}"#)
        .unwrap();
    writer.delete_document("users", &identifier(2)).unwrap();

    let root = fixture.root();
    assert!(root.is_database());
    assert_eq!(
        root.database_collection_names().unwrap(),
        vec!["users".to_owned(), "orders".to_owned()]
    );
    let users = root.collection("users").unwrap();
    assert_eq!(users.layout(), CollectionLayout::Segments);
    assert_eq!(users.document_ids().unwrap().len(), 11);
    assert_eq!(users.deleted_document_ids().unwrap(), vec![identifier(2)]);
    assert_eq!(read(&root, "users", 1)["name"], "renamed");
    assert_eq!(read(&root, "orders", 4)["name"], "orders4");
    assert_eq!(index_keys(&root, "users"), expected_keys(&root, "users"));
    assert_eq!(index_keys(&root, "orders"), expected_keys(&root, "orders"));
    // group 0 holds users 0, 3, 6 and 9.
    assert_eq!(
        find_by_index(&root, "users", "group", 3),
        [0, 3, 6, 9].map(identifier).to_vec()
    );
    fixture.assert_only_the_database();

    drop((writer, root, users));
    fixture.reopen();

    let root = fixture.root();
    assert_eq!(
        root.database_collection_names().unwrap(),
        vec!["users".to_owned(), "orders".to_owned()]
    );
    let users = root.collection("users").unwrap();
    assert_eq!(users.document_ids().unwrap().len(), 11);
    assert_eq!(users.deleted_document_ids().unwrap(), vec![identifier(2)]);
    assert_eq!(read(&root, "users", 1)["name"], "renamed");
    assert_eq!(read(&root, "users", 11)["name"], "users11");
    assert_eq!(read(&root, "orders", 0)["name"], "orders0");
    assert_eq!(index_keys(&root, "users"), expected_keys(&root, "users"));
    assert_eq!(
        find_by_index(&root, "users", "group", 3),
        [0, 3, 6, 9].map(identifier).to_vec()
    );
    assert_eq!(
        find_by_index(&root, "orders", "name", 4),
        vec![identifier(4)]
    );

    // A writer after the reopen continues where the first one stopped.
    let writer = fixture.writer();
    put(&writer, "orders", 7);
    let root = fixture.root();
    assert_eq!(
        root.collection("orders")
            .unwrap()
            .document_ids()
            .unwrap()
            .len(),
        6
    );
    assert_eq!(index_keys(&root, "orders"), expected_keys(&root, "orders"));
    fixture.assert_only_the_database();
}

/// A copy of the file taken while its writer is open is what a crash
/// leaves: the extent map predates the active segment. A reader follows
/// the segment the slot names, and the next writer settles it.
#[test]
fn a_database_left_by_a_stopped_writer_reads_and_writes_on() {
    let fixture = Fixture::create("stopped");
    let writer = fixture.writer();
    for name in ["users", "orders"] {
        writer
            .create_collection(name, CollectionKind::Document, None)
            .unwrap();
    }
    for number in 0..4 {
        put(&writer, "users", number);
        put(&writer, "orders", number);
    }
    let stopped = fixture.directory.join("stopped");
    fs::create_dir(&stopped).unwrap();
    let copy = stopped.join("copy.vdbx");
    fs::copy(fixture.database(), &copy).unwrap();
    drop(writer);

    let root = NativeRoot::open(&copy).unwrap();
    assert_eq!(
        root.collection("users")
            .unwrap()
            .document_ids()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(read(&root, "orders", 3)["name"], "orders3");
    assert_eq!(index_keys(&root, "users"), expected_keys(&root, "users"));

    let writer = NativeWriteRoot::open(&copy).unwrap();
    put(&writer, "users", 9);
    drop(writer);
    NativeRoot::close_database(&copy).unwrap();
    let root = NativeRoot::open(&copy).unwrap();
    assert_eq!(
        root.collection("users")
            .unwrap()
            .document_ids()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(read(&root, "users", 9)["name"], "users9");
    assert_eq!(
        find_by_index(&root, "users", "group", 0),
        [0, 3, 9].map(identifier).to_vec()
    );
    NativeRoot::close_database(&copy).unwrap();
}

#[test]
fn a_rebuild_writes_the_index_back_into_the_file() {
    let fixture = Fixture::create("rebuild");
    let writer = fixture.writer();
    writer
        .create_collection("users", CollectionKind::Document, None)
        .unwrap();
    for number in 0..6 {
        put(&writer, "users", number);
    }
    writer.rebuild_collection("users").unwrap();
    put(&writer, "users", 6);
    drop(writer);
    fixture.reopen();
    let root = fixture.root();
    assert_eq!(index_keys(&root, "users"), expected_keys(&root, "users"));
    assert_eq!(
        find_by_index(&root, "users", "group", 0),
        [0, 3, 6].map(identifier).to_vec()
    );
    fixture.assert_only_the_database();
}

#[test]
fn dropping_a_collection_keeps_the_others_and_frees_its_name() {
    let fixture = Fixture::create("drop");
    let writer = fixture.writer();
    for name in ["users", "orders"] {
        writer
            .create_collection(name, CollectionKind::Document, None)
            .unwrap();
        put(&writer, name, 1);
    }
    writer.drop_collection("users").unwrap();
    assert_eq!(
        code_of(fixture.root().collection("users")),
        NativeStorageErrorCode::NotFound
    );
    assert_eq!(
        code_of(writer.drop_collection("users")),
        NativeStorageErrorCode::NotFound
    );
    // The name is free again, and the new collection starts empty.
    assert!(
        writer
            .create_collection("users", CollectionKind::Document, None)
            .unwrap()
    );
    assert!(
        fixture
            .root()
            .collection("users")
            .unwrap()
            .document_ids()
            .unwrap()
            .is_empty()
    );
    put(&writer, "users", 2);
    drop(writer);
    fixture.reopen();
    let root = fixture.root();
    assert_eq!(
        root.database_collection_names().unwrap(),
        vec!["orders".to_owned(), "users".to_owned()]
    );
    assert_eq!(
        root.collection("users").unwrap().document_ids().unwrap(),
        vec![identifier(2)]
    );
    assert_eq!(read(&root, "orders", 1)["name"], "orders1");
    fixture.assert_only_the_database();
}

#[test]
fn a_database_refuses_what_it_does_not_hold() {
    let fixture = Fixture::create("refusals");
    let writer = fixture.writer();
    writer
        .create_collection("users", CollectionKind::Document, None)
        .unwrap();
    let unsupported = NativeStorageErrorCode::LayoutUnsupported;
    assert_eq!(unsupported.as_str(), "ELAYOUTUNSUPPORTED");
    assert_eq!(
        code_of(writer.create_collection("files", CollectionKind::File, None)),
        unsupported
    );
    assert_eq!(
        code_of(writer.create_collection_with_layout(
            "flat",
            CollectionKind::Document,
            None,
            Some(CollectionLayout::Files),
        )),
        unsupported
    );
    assert_eq!(
        code_of(writer.create_collection("history", CollectionKind::Document, Some(true))),
        unsupported
    );
    assert_eq!(
        code_of(writer.migrate_layout("users", CollectionLayout::Files)),
        unsupported
    );
    assert_eq!(code_of(writer.reshard_collection("users", 3)), unsupported);
    assert_eq!(code_of(writer.commit_if_dirty("snapshot")), unsupported);
    assert_eq!(code_of(writer.repository_branches()), unsupported);
    assert_eq!(code_of(fixture.root().version_history(10)), unsupported);
    assert_eq!(code_of(NativeQueue::open(fixture.database())), unsupported);
    assert_eq!(code_of(RootLease::acquire(fixture.database())), unsupported);
    // A missing root named `*.vdbx` is a database to create, never a
    // directory a lease makes.
    let missing = fixture.directory.join("missing.vdbx");
    assert_eq!(code_of(RootLease::acquire(&missing)), unsupported);
    assert!(!missing.exists());
    assert!(NativeRoot::names_database(&missing));
    assert!(NativeRoot::names_database(fixture.database()));
    assert!(!NativeRoot::names_database(&fixture.directory));
    // A file that is not a database is refused as one.
    let other = fixture.directory.join("notes.txt");
    fs::write(&other, b"not a database").unwrap();
    assert_eq!(
        code_of(NativeRoot::open(&other)),
        NativeStorageErrorCode::WrongType
    );
    fs::remove_file(&other).unwrap();
    fixture.assert_only_the_database();
}

#[test]
fn creating_over_an_existing_path_fails() {
    let fixture = Fixture::create("exists");
    assert!(NativeRoot::create_database(fixture.database()).is_err());
    assert!(NativeRoot::open(fixture.database()).unwrap().is_database());
    let directory: &Path = &fixture.directory;
    assert!(!NativeRoot::open(directory).unwrap().is_database());
}

const CHILD_ROLE: &str = "YCKR_VDBX_ROOT_CHILD_ROLE";
const CHILD_DATABASE: &str = "YCKR_VDBX_ROOT_CHILD_DATABASE";
const CHILD_COLLECTION: &str = "YCKR_VDBX_ROOT_CHILD_COLLECTION";
const CHILD_FIRST: &str = "YCKR_VDBX_ROOT_CHILD_FIRST";
const CHILD_TEST: &str = "a_child_process";
const OLDER_TEST_EXE: &str = "YCKR_VDBX_OLDER_TEST_EXE";
/// Documents each writer process puts, of about 48 KiB each once
/// compressed, so the extents of a database fill and every writer takes
/// several.
const CHILD_DOCUMENTS: usize = 200;
/// Characters of a child document's padding: 48 KiB once compressed.
const PADDING_CHARS: usize = 64 * 1024;
const WAIT: Duration = Duration::from_mins(2);
/// The longest a reader child reads before it stops by itself.
const READER_LIMIT: Duration = Duration::from_mins(10);

fn padded(collection: &str, number: usize) -> Value {
    let mut value = body(collection, number);
    value["padding"] = Value::from(byckr_test_support::incompressible_text(
        u64::try_from(number).unwrap(),
        PADDING_CHARS,
    ));
    value
}

/// What a child process does, by role; nothing when it is not a child.
///
/// - `write`: put [`CHILD_DOCUMENTS`] documents into its collection,
///   starting at its first number, once the parent says `go`.
/// - `read`: read the database again and again while the writers write,
///   as a reader that takes no lock, until the parent says `done`.
#[test]
fn a_child_process() {
    let Ok(role) = std::env::var(CHILD_ROLE) else {
        return;
    };
    let database = PathBuf::from(std::env::var(CHILD_DATABASE).unwrap());
    let directory = database.parent().unwrap().to_owned();
    match role.as_str() {
        "write" => {
            let collection = std::env::var(CHILD_COLLECTION).unwrap();
            let first: usize = std::env::var(CHILD_FIRST).unwrap().parse().unwrap();
            let writer = NativeWriteRoot::open(&database).unwrap();
            fs::write(directory.join(format!("{collection}{first}.ready")), b"").unwrap();
            wait_for(&directory.join("go"));
            for number in first..first + CHILD_DOCUMENTS {
                put_or_explain(&writer, &database, &collection, number);
            }
        }
        "fail" => {
            let collection = std::env::var(CHILD_COLLECTION).unwrap();
            let first = std::env::var(CHILD_FIRST).unwrap();
            fs::write(directory.join(format!("{collection}{first}.ready")), b"").unwrap();
            wait_for(&directory.join("go"));
            panic!("this writer fails on purpose");
        }
        "read" => read_until_done(&database, &directory),
        other => panic!("unknown child role {other}"),
    }
}

/// Put one padded document as a writer child. When the put fails, panic
/// with what a later reader of the log needs: the error with its code, its
/// OS error and message, how long the put took, and the lock bytes some
/// holder had at that moment.
fn put_or_explain(writer: &NativeWriteRoot, database: &Path, collection: &str, number: usize) {
    let started = Instant::now();
    let Err(error) = writer.put_document(
        collection,
        &identifier(number),
        padded(collection, number).to_string().as_bytes(),
        PutDocumentOptions::default(),
    ) else {
        return;
    };
    let elapsed = started.elapsed();
    panic!(
        "put_document of {collection} {number} failed after {elapsed:?}: {error} \
         (code {}, {error:?}, source {:?}); lock bytes held then: {:?}",
        error.code().as_str(),
        std::error::Error::source(&error),
        held_lock_bytes(database)
    );
}

/// The allocation byte, the writer bytes of the first directory blocks and
/// their turn bytes that a holder, this process or another, has now.
fn held_lock_bytes(database: &Path) -> Vec<String> {
    let file = ByteRangeLockFile::open(database).unwrap();
    std::iter::once(LockByte::ALLOCATION)
        .chain((0..8).map(LockByte::collection_writer))
        .flat_map(|byte| {
            std::iter::once(byte).chain((1..=TURN_LEVELS).map(move |level| byte.turn(level)))
        })
        .filter(|byte| file.try_lock(*byte, LockMode::Exclusive).unwrap().is_none())
        .map(|byte| byte.to_string())
        .collect()
}

/// The reader child: read every document again and again until the parent
/// says `done`, for at least three rounds, counting each read in
/// `reader.reads`. It stops by itself after [`READER_LIMIT`], so a reader
/// whose parent was stopped without saying `done` does not run on.
fn read_until_done(database: &Path, directory: &Path) {
    let root = NativeRoot::open(database).unwrap();
    fs::write(directory.join("reader.ready"), b"").unwrap();
    wait_for(&directory.join("go"));
    let deadline = Instant::now() + READER_LIMIT;
    let mut reads = 0_u64;
    let mut rounds = 0;
    while rounds < 3 || !directory.join("done").exists() {
        assert!(Instant::now() < deadline, "the parent never said done");
        for name in ["users", "orders"] {
            let collection = root.collection(name).unwrap();
            for id in collection.document_ids().unwrap() {
                let bytes = collection.read_document(&id).unwrap().bytes;
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(value["padding"].as_str().map(str::len), Some(PADDING_CHARS));
                reads += 1;
                fs::write(directory.join("reader.reads"), reads.to_string()).unwrap();
            }
        }
        rounds += 1;
    }
}

/// Who the second writer of `users` is.
enum SecondUsersWriter<'a> {
    /// This test binary, like the other writers.
    Current,
    /// A test binary built from an older BYCKR.
    Older(&'a Path),
    /// This test binary, failing on purpose once the parent says `go`.
    Failing,
}

impl SecondUsersWriter<'_> {
    /// The program the writer runs, and its role.
    fn program_and_role<'b>(&'b self, current: &'b Path) -> (&'b Path, &'static str) {
        match self {
            Self::Current => (current, "write"),
            Self::Older(older) => (older, "write"),
            Self::Failing => (current, "fail"),
        }
    }
}

/// Three writers, two of them on one collection, and a reader, all on
/// `database`.
fn spawn_writers_and_reader(database: &Path, second: &SecondUsersWriter) -> Children {
    let current = std::env::current_exe().unwrap();
    let (program, role) = second.program_and_role(&current);
    let mut children = Children(
        [
            ("users", 0, current.as_path(), "write"),
            ("orders", 0, current.as_path(), "write"),
            ("users", 1_000, program, role),
        ]
        .into_iter()
        .map(|(collection, first, program, role)| {
            let first = first.to_string();
            let child = spawn(
                program,
                role,
                database,
                &[(CHILD_COLLECTION, collection), (CHILD_FIRST, &first)],
            );
            (format!("{collection}{first}"), Some(child))
        })
        .collect(),
    );
    let reader = spawn(&current, "read", database, &[]);
    children.0.push(("reader".to_owned(), Some(reader)));
    children
}

fn spawn(program: &Path, role: &str, database: &Path, environment: &[(&str, &str)]) -> Child {
    Command::new(program)
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD_ROLE, role)
        .env(CHILD_DATABASE, database)
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

fn wait_for(path: &Path) {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether `fixture`'s filesystem has qualified byte-range locks, saying
/// why the test is skipped when it has not.
fn locks_qualified(fixture: &Fixture) -> bool {
    let filesystem = FilesystemInfo::of(&fs::File::open(fixture.database()).unwrap()).unwrap();
    let qualified = filesystem.byte_range_locks_qualified();
    if !qualified {
        eprintln!("skipped: byte-range locks are not qualified on {filesystem:?}");
    }
    qualified
}

/// Create `users` and `orders`, then run three writer processes and a
/// reader on the database until every writer has passed. Nothing in this
/// process holds the file while they run.
fn write_from_processes(fixture: &Fixture, second: &SecondUsersWriter) {
    let writer = fixture.writer();
    for name in ["users", "orders"] {
        writer
            .create_collection(name, CollectionKind::Document, None)
            .unwrap();
    }
    drop(writer);
    fixture.reopen();

    let mut children = spawn_writers_and_reader(&fixture.database(), second);
    for (name, child) in children.named() {
        let ready = fixture.directory.join(format!("{name}.ready"));
        let deadline = Instant::now() + WAIT;
        while !ready.exists() {
            assert!(child.try_wait().unwrap().is_none(), "{name} stopped early");
            assert!(Instant::now() < deadline, "{name} never got ready");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fs::write(fixture.directory.join("go"), b"").unwrap();
    let reader = children.0.len() - 1;
    for index in 0..reader {
        let (name, child) = children.take(index);
        passed(child, &name);
    }
    fs::write(fixture.directory.join("done"), b"").unwrap();
    let (_, child) = children.take(reader);
    passed(child, "reader");
}

/// What the writers of [`write_from_processes`] left: every document and
/// index entry, in extents of their own. Returns the ids of `users`.
fn assert_the_writers_wrote(fixture: &Fixture) -> Vec<String> {
    // Each writer took extents of its own: users holds about 19 MiB and
    // orders about 10 MiB of 2 MiB extents, every finished one sealed.
    let report = ContainerFile::open_reader(&fixture.database())
        .unwrap()
        .report()
        .unwrap();
    let segments = report.extents.by_kind["segment"];
    assert!(segments.extents >= 5, "{report:?}");
    assert!(segments.sealed >= 3, "{report:?}");
    let root = fixture.root();
    let users = root.collection("users").unwrap().document_ids().unwrap();
    let orders = root.collection("orders").unwrap().document_ids().unwrap();
    assert_eq!(users.len(), 2 * CHILD_DOCUMENTS);
    assert_eq!(orders.len(), CHILD_DOCUMENTS);
    for number in (0..CHILD_DOCUMENTS).step_by(37) {
        assert_eq!(
            read(&root, "users", number)["name"],
            format!("users{number}")
        );
        assert_eq!(
            read(&root, "users", 1_000 + number)["name"],
            format!("users{}", 1_000 + number)
        );
        assert_eq!(
            read(&root, "orders", number)["name"],
            format!("orders{number}")
        );
    }
    for name in ["users", "orders"] {
        assert_eq!(
            index_keys(&root, name),
            expected_keys(&root, name),
            "{name}"
        );
    }
    users
}

/// Remove the files the parent and the children signalled with, and the
/// reader's count of reads.
fn remove_signals(directory: &Path) {
    for name in [".ready", "go", "done", "reader.reads"] {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.to_string_lossy().ends_with(name) {
                fs::remove_file(path).unwrap();
            }
        }
    }
}

/// Writer processes of two collections of one database, and two of one
/// collection, write side by side under byte-range locks while a reader
/// process reads: every process takes extents from the one map, under the
/// allocation byte, and the writers of one collection take turns on its
/// writer byte.
#[test]
fn writer_processes_share_a_database_under_byte_range_locks() {
    let fixture = Fixture::create("processes");
    if !locks_qualified(&fixture) {
        return;
    }
    write_from_processes(&fixture, &SecondUsersWriter::Current);
    let users = assert_the_writers_wrote(&fixture);
    // A new process finds the same: the map the writers left is whole.
    fixture.reopen();
    let root = fixture.root();
    assert_eq!(
        root.collection("users").unwrap().document_ids().unwrap(),
        users
    );
    let writer = fixture.writer();
    put(&writer, "orders", 5_000);
    assert_eq!(read(&root, "orders", 5_000)["name"], "orders5000");
    drop((writer, root));
    remove_signals(&fixture.directory);
    fixture.assert_only_the_database();
}

/// A writer process of an older BYCKR, which waits for lock bytes without
/// taking turns (ADR 0016 section 7), shares the database with newer
/// writers of the same collection and of another: every document arrives,
/// and nothing is lost or damaged. `YCKR_VDBX_OLDER_TEST_EXE` names this
/// test binary built from the older BYCKR; without it the test is skipped.
#[test]
fn an_older_writer_process_shares_a_database_with_newer_ones() {
    let Ok(older) = std::env::var(OLDER_TEST_EXE) else {
        eprintln!("skipped: {OLDER_TEST_EXE} names no older test binary");
        return;
    };
    let fixture = Fixture::create("older");
    if !locks_qualified(&fixture) {
        return;
    }
    write_from_processes(&fixture, &SecondUsersWriter::Older(Path::new(&older)));
    assert_the_writers_wrote(&fixture);
    fixture.reopen();
    remove_signals(&fixture.directory);
    fixture.assert_only_the_database();
}

/// A writer child that fails makes the test fail, and leaves no child
/// process running: the reader, which reads until the parent says `done`,
/// is stopped and reaped when the parent's assertion fails.
#[test]
fn a_failing_writer_child_leaves_no_child_process_running() {
    let fixture = Fixture::create("failing");
    if !locks_qualified(&fixture) {
        return;
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        write_from_processes(&fixture, &SecondUsersWriter::Failing);
    }));
    assert!(outcome.is_err(), "the failing writer was not reported");
    let reads = fixture.directory.join("reader.reads");
    let before = fs::read_to_string(&reads).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        fs::read_to_string(&reads).unwrap(),
        before,
        "the reader child still runs"
    );
}

/// How long one writer alone takes per `put_document`, at the 50th and the
/// 99th percentile, with nothing else using the database. A measurement,
/// run on purpose; the test binary built from an older BYCKR runs it too.
#[test]
#[ignore = "a measurement: run with --run-ignored"]
fn measure_put_document_for_one_writer_alone() {
    let fixture = Fixture::create("put-cost");
    let writer = fixture.writer();
    writer
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    let mut times: Vec<Duration> = (0..2_000)
        .map(|number| timed_put(&writer, number))
        .collect();
    times.sort();
    eprintln!(
        "put_document, one writer alone: p50 {:?}, p99 {:?}, of {} puts",
        times[times.len() / 2],
        times[times.len() * 99 / 100],
        times.len()
    );
    assert_eq!(times.len(), 2_000);
}

fn timed_put(writer: &NativeWriteRoot, number: usize) -> Duration {
    let started = Instant::now();
    put(writer, "items", number);
    started.elapsed()
}
