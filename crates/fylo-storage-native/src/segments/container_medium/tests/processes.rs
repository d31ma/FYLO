//! Writers and readers of one container in separate processes (ADR 0016
//! section 7). The parent tests spawn this test binary again, running
//! [`a_child_process`] in the role an environment variable names.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{ContainerMedium, Fixture, identifier, put_into, writer_at};
use crate::{
    CollectionKind, ContainerLeaseTiming, ContainerWriteAccess, ContainerWriteOptions,
    FilesystemInfo, NativeRoot, NativeStorageErrorCode,
};

const ROLE: &str = "FYLO_VDBX_CHILD_ROLE";
const ROOT: &str = "FYLO_VDBX_CHILD_ROOT";
const COLLECTION: &str = "FYLO_VDBX_CHILD_COLLECTION";
const EXPECT: &str = "FYLO_VDBX_CHILD_EXPECT";
const CHILD_TEST: &str = "segments::container_medium::tests::processes::a_child_process";
const WAIT: Duration = Duration::from_mins(1);

/// What a child process does, by role; nothing when it is not a child.
///
/// - `hold`: write one document in exclusive mode (the lease), say so, and
///   keep the medium, and so the lease, until the parent says `release`.
/// - `write`: try to write in exclusive mode, and expect `EBUSY`.
/// - `read`: read every committed document, as a reader that takes no lock.
/// - `commit`: write five documents to its collection where byte-range
///   locks are used, say so, wait for `go`, and write five more while the
///   other writer process holds its own collection.
#[test]
fn a_child_process() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let root = PathBuf::from(std::env::var(ROOT).unwrap());
    let collection = std::env::var(COLLECTION).unwrap_or_else(|_| "items".into());
    let directory = root.join(".collections").join(&collection);
    let exclusive = role != "commit";
    ContainerMedium::set_write_options(ContainerWriteOptions {
        busy_timeout: Duration::from_millis(300),
        exclusive,
        lease_timing: ContainerLeaseTiming::default(),
    });
    let writer = writer_at(&root);
    match role.as_str() {
        "hold" => {
            let medium = ContainerMedium::of(&directory).unwrap().unwrap();
            put_into(&writer, &collection, 100).unwrap();
            assert!(matches!(
                medium
                    .shared
                    .file()
                    .access
                    .as_ref()
                    .map(|right| &right.access),
                Some(ContainerWriteAccess::Lease { .. })
            ));
            signal(&root, "hold");
            wait_for(&root.join("release"));
        }
        "write" => {
            let started = Instant::now();
            let error = put_into(&writer, &collection, 200).unwrap_err();
            assert_eq!(error.code(), NativeStorageErrorCode::RootLocked, "{error}");
            assert!(error.to_string().contains("EBUSY"), "{error}");
            assert!(started.elapsed() >= Duration::from_millis(300));
        }
        "read" => {
            let expected: usize = std::env::var(EXPECT).unwrap().parse().unwrap();
            let read = NativeRoot::open(&root)
                .unwrap()
                .collection(&collection)
                .unwrap();
            let ids = read.document_ids().unwrap();
            assert_eq!(ids.len(), expected);
            for id in ids {
                let bytes = read.read_document(&id).unwrap().bytes;
                assert!(!bytes.is_empty());
            }
            let medium = ContainerMedium::of(&directory).unwrap().unwrap();
            assert!(!medium.shared.file().is_writable(), "a reader never writes");
        }
        "commit" => {
            let medium = ContainerMedium::of(&directory).unwrap().unwrap();
            for number in 0..5 {
                put_into(&writer, &collection, number).unwrap();
            }
            assert!(matches!(
                medium
                    .shared
                    .file()
                    .access
                    .as_ref()
                    .map(|right| &right.access),
                Some(ContainerWriteAccess::SharedLocks { .. })
            ));
            signal(&root, &collection);
            wait_for(&root.join("go"));
            for number in 5..10 {
                put_into(&writer, &collection, number).unwrap();
            }
        }
        other => panic!("unknown child role {other}"),
    }
}

fn spawn(role: &str, root: &Path, environment: &[(&str, &str)]) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(ROOT, root)
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

fn signal(root: &Path, name: &str) {
    std::fs::write(root.join(format!("{name}.ready")), b"").unwrap();
}

/// Wait for `path` to exist, or for a child that should create it to stop.
fn wait_until(path: &Path, children: &mut [&mut Child]) {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        for child in children.iter_mut() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "a child stopped before {}",
                path.display()
            );
        }
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for(path: &Path) {
    wait_until(path, &mut []);
}

#[test]
fn a_second_writer_process_is_busy_under_the_lease_while_a_reader_reads() {
    let fixture = Fixture::create("vdbx-medium-lease");
    let writer = fixture.writer();
    for number in 0..5 {
        put_into(&writer, "items", number).unwrap();
    }
    // Nothing in this process writes the file from here on.
    fixture.crash();

    let mut holder = spawn("hold", &fixture.0, &[]);
    wait_until(&fixture.0.join("hold.ready"), &mut [&mut holder]);
    let lease = std::fs::read_dir(fixture.0.join(".collections/items"))
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().contains(".lease."));
    assert!(lease, "the holder writes under a lease file");

    passed(spawn("write", &fixture.0, &[]), "write");
    passed(spawn("read", &fixture.0, &[(EXPECT, "6")]), "read");

    std::fs::write(fixture.0.join("release"), b"").unwrap();
    passed(holder, "hold");
    let bodies = fixture.bodies();
    assert_eq!(bodies.len(), 6);
    assert!(bodies.contains_key(&identifier(100)));
    assert!(!bodies.contains_key(&identifier(200)));
}

#[test]
fn a_writer_takes_over_at_once_from_a_killed_writer_process_on_this_host() {
    let fixture = Fixture::create("vdbx-medium-killed");
    put_into(&fixture.writer(), "items", 0).unwrap();
    fixture.crash();

    let mut holder = spawn("hold", &fixture.0, &[]);
    wait_until(&fixture.0.join("hold.ready"), &mut [&mut holder]);
    holder.kill().unwrap();
    holder.wait().unwrap();
    // The lease the killed writer left is not waited out for its takeover
    // time (30 s), which is longer than the busy timeout (5 s).
    let started = Instant::now();
    put_into(&fixture.writer(), "items", 1).unwrap();
    assert!(started.elapsed() < ContainerWriteOptions::default().busy_timeout);
    let bodies = fixture.bodies();
    assert_eq!(bodies.len(), 3);
    assert!(bodies.contains_key(&identifier(100)));
}

#[test]
fn writer_processes_of_two_collections_commit_side_by_side_under_locks() {
    let fixture = Fixture::create("vdbx-medium-locks");
    fixture
        .writer()
        .create_collection("others", CollectionKind::Document, None)
        .unwrap();
    let file = std::fs::File::open(fixture.0.join(".collections/items/segments.vdbx")).unwrap();
    let filesystem = FilesystemInfo::of(&file).unwrap();
    if !filesystem.byte_range_locks_qualified() {
        eprintln!("skipped: byte-range locks are not qualified on {filesystem:?}");
        return;
    }
    fixture.crash();
    ContainerMedium::forget(&fixture.0.join(".collections/others"));

    let mut items = spawn("commit", &fixture.0, &[(COLLECTION, "items")]);
    let mut others = spawn("commit", &fixture.0, &[(COLLECTION, "others")]);
    wait_until(
        &fixture.0.join("items.ready"),
        &mut [&mut items, &mut others],
    );
    wait_until(
        &fixture.0.join("others.ready"),
        &mut [&mut items, &mut others],
    );
    std::fs::write(fixture.0.join("go"), b"").unwrap();
    passed(items, "commit items");
    passed(others, "commit others");

    let root = NativeRoot::open(&fixture.0).unwrap();
    for name in ["items", "others"] {
        let collection = root.collection(name).unwrap();
        assert_eq!(collection.document_ids().unwrap().len(), 10, "{name}");
    }
    ContainerMedium::forget(&fixture.0.join(".collections/others"));
}
