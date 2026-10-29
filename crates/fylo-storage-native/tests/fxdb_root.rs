//! A `.fxdb` file opened as a FYLO root (ADR 0016 section 11,
//! `docs/fxdb-segment-backend.md`, "Root integration"): every collection
//! lives in the one file, and nothing is created beside it but the lease
//! file a writer holds.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fylo_query::{IndexLookupValue, QueryLimits, ScanQuery, index_entries_for_document};
use fylo_storage_native::{
    CollectionKind, CollectionLayout, ContainerFile, FilesystemInfo, NativeQueue, NativeRoot,
    NativeStorageErrorCode, NativeWriteRoot, PutDocumentOptions, RootLease,
};
use serde_json::{Map, Value, json};

struct Fixture {
    directory: PathBuf,
}

impl Fixture {
    fn create(label: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "fylo-fxdb-root-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let fixture = Self {
            directory: fs::canonicalize(directory).unwrap(),
        };
        NativeRoot::create_database(fixture.database()).unwrap();
        fixture
    }

    fn database(&self) -> PathBuf {
        self.directory.join("db.fxdb")
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
        assert!(names.contains(&"db.fxdb".to_owned()), "{names:?}");
        let others: Vec<&String> = names
            .iter()
            .filter(|name| name.as_str() != "db.fxdb")
            .collect();
        assert!(
            others.len() <= 1 && others.iter().all(|name| name.starts_with("db.fxdb.lease.")),
            "unexpected entries beside the database: {names:?}"
        );
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
    result: Result<T, fylo_storage_native::NativeStorageError>,
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
        .patch_document("users", &identifier(1), br#"{"name":"renamed"}"#, None)
        .unwrap();
    writer
        .delete_document("users", &identifier(2), None)
        .unwrap();

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
    let copy = stopped.join("copy.fxdb");
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
        code_of(writer.migrate_layout("users", CollectionLayout::Files, false)),
        unsupported
    );
    assert_eq!(code_of(writer.reshard_collection("users", 3)), unsupported);
    assert_eq!(code_of(writer.commit_if_dirty("snapshot")), unsupported);
    assert_eq!(code_of(writer.repository_branches()), unsupported);
    assert_eq!(code_of(fixture.root().version_history(10)), unsupported);
    assert_eq!(code_of(NativeQueue::open(fixture.database())), unsupported);
    assert_eq!(code_of(RootLease::acquire(fixture.database())), unsupported);
    // A missing root named `*.fxdb` is a database to create, never a
    // directory a lease makes.
    let missing = fixture.directory.join("missing.fxdb");
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

const CHILD_ROLE: &str = "FYLO_FXDB_ROOT_CHILD_ROLE";
const CHILD_DATABASE: &str = "FYLO_FXDB_ROOT_CHILD_DATABASE";
const CHILD_COLLECTION: &str = "FYLO_FXDB_ROOT_CHILD_COLLECTION";
const CHILD_FIRST: &str = "FYLO_FXDB_ROOT_CHILD_FIRST";
const CHILD_TEST: &str = "a_child_process";
/// Documents each writer process puts, of about 48 KiB each once
/// compressed, so the 8 MiB extents of a database fill and every writer
/// takes several.
const CHILD_DOCUMENTS: usize = 200;
/// Characters of a child document's padding: 48 KiB once compressed.
const PADDING_CHARS: usize = 64 * 1024;
const WAIT: Duration = Duration::from_mins(2);

fn padded(collection: &str, number: usize) -> Value {
    let mut value = body(collection, number);
    value["padding"] = Value::from(fylo_test_support::incompressible_text(PADDING_CHARS));
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
                writer
                    .put_document(
                        &collection,
                        &identifier(number),
                        padded(&collection, number).to_string().as_bytes(),
                        PutDocumentOptions::default(),
                    )
                    .unwrap();
            }
        }
        "read" => {
            let root = NativeRoot::open(&database).unwrap();
            fs::write(directory.join("reader.ready"), b"").unwrap();
            wait_for(&directory.join("go"));
            let mut rounds = 0;
            while rounds < 3 || !directory.join("done").exists() {
                for name in ["users", "orders"] {
                    let collection = root.collection(name).unwrap();
                    for id in collection.document_ids().unwrap() {
                        let bytes = collection.read_document(&id).unwrap().bytes;
                        let value: Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(value["padding"].as_str().map(str::len), Some(PADDING_CHARS));
                    }
                }
                rounds += 1;
            }
        }
        other => panic!("unknown child role {other}"),
    }
}

fn spawn(role: &str, database: &Path, environment: &[(&str, &str)]) -> Child {
    Command::new(std::env::current_exe().unwrap())
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

/// Writer processes of two collections of one database, and two of one
/// collection, write side by side under byte-range locks while a reader
/// process reads: every process takes extents from the one map, under the
/// allocation byte, and the writers of one collection take turns on its
/// writer byte.
#[test]
fn writer_processes_share_a_database_under_byte_range_locks() {
    let fixture = Fixture::create("processes");
    let filesystem = FilesystemInfo::of(&fs::File::open(fixture.database()).unwrap()).unwrap();
    if !filesystem.byte_range_locks_qualified() {
        eprintln!("skipped: byte-range locks are not qualified on {filesystem:?}");
        return;
    }
    let writer = fixture.writer();
    for name in ["users", "orders"] {
        writer
            .create_collection(name, CollectionKind::Document, None)
            .unwrap();
    }
    drop(writer);
    // Nothing in this process holds the file from here on.
    fixture.reopen();

    let database = fixture.database();
    let mut children: Vec<(String, Child)> = [("users", 0), ("orders", 0), ("users", 1_000)]
        .into_iter()
        .map(|(collection, first)| {
            let first = first.to_string();
            let child = spawn(
                "write",
                &database,
                &[(CHILD_COLLECTION, collection), (CHILD_FIRST, &first)],
            );
            (format!("{collection}{first}"), child)
        })
        .collect();
    children.push(("reader".to_owned(), spawn("read", &database, &[])));
    for (name, child) in &mut children {
        let ready = fixture.directory.join(format!("{name}.ready"));
        let deadline = Instant::now() + WAIT;
        while !ready.exists() {
            assert!(child.try_wait().unwrap().is_none(), "{name} stopped early");
            assert!(Instant::now() < deadline, "{name} never got ready");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fs::write(fixture.directory.join("go"), b"").unwrap();
    let reader = children.pop().unwrap();
    for (name, child) in children {
        passed(child, &name);
    }
    fs::write(fixture.directory.join("done"), b"").unwrap();
    passed(reader.1, "reader");

    // Each writer took extents of its own: users holds about 19 MiB and
    // orders about 10 MiB of 8 MiB extents, every finished one sealed.
    let report = ContainerFile::open_reader(&database)
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
    drop(root);
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
    for name in [".ready", "go", "done"] {
        for entry in fs::read_dir(&fixture.directory).unwrap() {
            let path = entry.unwrap().path();
            if path.to_string_lossy().ends_with(name) {
                fs::remove_file(path).unwrap();
            }
        }
    }
    fixture.assert_only_the_database();
}
