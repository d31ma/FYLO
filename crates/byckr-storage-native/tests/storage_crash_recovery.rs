//! What BYCKR does after a storage crash that lost or reordered writes
//! (L1 spec 20.2 and 20.3): a damaged file is refused with its path, root
//! verification lists every damaged file, and a copy of a root taken at any
//! instant of a write load recovers to a clean root with every reported
//! commit present.
//!
//! The damage is made by hand, the way an `async` NFS export can leave a
//! file after its server crashed: a lost tail, a torn file that kept its
//! size and time, and a rename that arrived without its data.

use std::fs;
use std::path::{Path, PathBuf};

use byckr_storage_native::{
    CollectionKind, NativeRoot, NativeWriteRoot, PutDocumentOptions, PutRawFileOptions,
    RootVerification, RootVerificationReport,
};

const ITEMS: &str = "items";
const FILES: &str = "files";

struct Scratch {
    folder: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let folder = byckr_test_support::TestFolder::path().join(format!(
            "fylo-crash-recovery-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&folder).unwrap();
        Self {
            folder: fs::canonicalize(folder).unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.folder.join("db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.folder);
    }
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 30_000 + number)
}

fn put(writer: &NativeWriteRoot, number: usize) {
    let body = format!("{{\"name\":\"item{number}\",\"n\":{number}}}");
    writer
        .put_document(
            ITEMS,
            &identifier(number),
            body.as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn seeded(root: &Path) -> NativeWriteRoot {
    fs::create_dir_all(root).unwrap();
    let writer = NativeWriteRoot::open(root).unwrap();
    writer
        .create_collection(ITEMS, CollectionKind::Document, None)
        .unwrap();
    writer
        .create_collection(FILES, CollectionKind::File, None)
        .unwrap();
    for number in 0..5 {
        put(&writer, number);
    }
    let options = PutRawFileOptions {
        key: "/photos/one.bin".into(),
        extension: ".bin".into(),
        ..PutRawFileOptions::default()
    };
    writer
        .put_raw_file(FILES, &identifier(0), b"0123456789abcdef", &options)
        .unwrap();
    writer
}

fn verify(root: &Path) -> RootVerificationReport {
    let writer = NativeWriteRoot::open(root).unwrap();
    let reader = NativeRoot::open(root).unwrap();
    RootVerification::run(&writer, &reader)
}

fn document_path(root: &Path, number: usize) -> PathBuf {
    NativeRoot::open(root)
        .unwrap()
        .collection(ITEMS)
        .unwrap()
        .read_document(&identifier(number))
        .unwrap()
        .path
}

fn raw_file_path(root: &Path) -> PathBuf {
    NativeRoot::open(root)
        .unwrap()
        .collection(FILES)
        .unwrap()
        .read_raw_file(&identifier(0))
        .unwrap()
        .path
}

fn named(report: &RootVerificationReport, path: &Path) -> Vec<String> {
    report
        .damaged
        .iter()
        .filter(|damage| damage.file == path.display().to_string())
        .map(|damage| damage.code.clone())
        .collect()
}

/// Rewrite a file in place and give it back its old modification time, as
/// a torn write that kept its size and time leaves it.
fn tear_in_place(path: &Path, bytes: &[u8]) {
    let modified = fs::metadata(path).unwrap().modified().unwrap();
    fs::write(path, bytes).unwrap();
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(modified).unwrap();
}

#[test]
fn a_clean_root_verifies_clean() {
    let scratch = Scratch::new("clean");
    drop(seeded(&scratch.root()));
    let report = verify(&scratch.root());
    assert!(report.ok, "{report:?}");
    assert_eq!(report.collections_checked, 2);
    assert_eq!(report.files_checked, 6);
}

#[test]
fn a_document_that_lost_its_tail_is_listed_with_its_path() {
    let scratch = Scratch::new("lost-tail");
    drop(seeded(&scratch.root()));
    let path = document_path(&scratch.root(), 2);
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    let report = verify(&scratch.root());
    assert!(!report.ok);
    assert_eq!(named(&report, &path), ["ENATIVE_DOCUMENT"], "{report:?}");
    assert!(
        report.damaged[0]
            .message
            .contains(&path.display().to_string())
    );
}

#[test]
fn a_document_renamed_without_its_data_is_listed() {
    let scratch = Scratch::new("empty-rename");
    drop(seeded(&scratch.root()));
    let path = document_path(&scratch.root(), 3);
    fs::write(&path, b"").unwrap();
    let report = verify(&scratch.root());
    assert_eq!(named(&report, &path), ["ENATIVE_DOCUMENT"], "{report:?}");
}

#[test]
fn a_document_the_index_names_but_that_is_gone_is_reported_as_an_index_mismatch() {
    let scratch = Scratch::new("missing");
    drop(seeded(&scratch.root()));
    fs::remove_file(document_path(&scratch.root(), 1)).unwrap();
    let report = verify(&scratch.root());
    assert!(!report.ok);
    assert!(
        report
            .damaged
            .iter()
            .any(|damage| damage.collection == ITEMS && damage.code == "ENATIVE_INDEX"),
        "{report:?}"
    );
}

#[test]
fn a_torn_file_that_kept_its_size_and_time_is_refused_on_read_with_its_path() {
    let scratch = Scratch::new("torn-file");
    drop(seeded(&scratch.root()));
    let path = raw_file_path(&scratch.root());
    tear_in_place(&path, b"0123456789XXXXXX");
    let refused = NativeRoot::open(scratch.root())
        .unwrap()
        .collection(FILES)
        .unwrap()
        .read_raw_file(&identifier(0))
        .unwrap_err();
    assert_eq!(refused.code().as_str(), "ENATIVE_METADATA");
    assert!(
        refused.to_string().contains(&path.display().to_string()),
        "{refused}"
    );
    let report = verify(&scratch.root());
    assert_eq!(report.damaged.len(), 1, "{report:?}");
    assert_eq!(report.damaged[0].collection, FILES);
}

#[test]
fn rebuilding_the_collection_after_a_restore_makes_the_root_verify_clean() {
    let scratch = Scratch::new("restore");
    drop(seeded(&scratch.root()));
    let path = document_path(&scratch.root(), 1);
    let backup = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(!verify(&scratch.root()).ok);
    fs::write(&path, backup).unwrap();
    NativeWriteRoot::open(scratch.root())
        .unwrap()
        .rebuild_collection(ITEMS)
        .unwrap();
    assert!(verify(&scratch.root()).ok);
}

/// L1 spec 20.3 needs a process to freeze, which these tests do with Unix
/// signals.
#[cfg(unix)]
mod snapshots {
    use super::{ITEMS, Scratch, identifier, put, seeded, verify};
    use byckr_storage_native::{NativeRoot, NativeWriteRoot};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    const CHILD: &str = "YCKR_CRASH_RECOVERY_CHILD";
    const LOG: &str = "YCKR_CRASH_RECOVERY_LOG";
    const SNAPSHOTS: usize = 6;
    const LOAD_WAIT: Duration = Duration::from_mins(1);

    /// The writer of the snapshot test: puts documents one after another and
    /// appends each id to the log once its put returned. Does nothing in an
    /// ordinary run.
    #[test]
    fn crash_recovery_child() {
        let Ok(root) = std::env::var(CHILD) else {
            return;
        };
        let root = PathBuf::from(root);
        let log = PathBuf::from(std::env::var(LOG).unwrap());
        let writer = NativeWriteRoot::open(&root).unwrap();
        let mut file = fs::File::options()
            .append(true)
            .create(true)
            .open(&log)
            .unwrap();
        for number in 100.. {
            put(&writer, number);
            std::io::Write::write_all(&mut file, format!("{}\n", identifier(number)).as_bytes())
                .unwrap();
            file.sync_all().unwrap();
        }
    }

    fn signal(pid: u32, name: &str) {
        let status = Command::new("kill")
            .args([&format!("-{name}"), &pid.to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn copy_tree(from: &Path, to: &Path) {
        // `-a` keeps extended attributes on Linux too (a raw file's key is
        // one), as a filesystem snapshot does; `-Rp` drops them there.
        let status = Command::new("cp")
            .arg("-a")
            .arg(from)
            .arg(to)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn wait_for_lines(log: &Path, count: usize) {
        let started = Instant::now();
        while fs::read_to_string(log).map_or(0, |text| text.lines().count()) < count {
            assert!(
                started.elapsed() < LOAD_WAIT,
                "the writer never wrote {count} documents"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// L1 spec 20.3: a snapshot is a power cut at one instant. The writer is
    /// frozen at random moments; each frozen root is copied, and every copy
    /// must recover, verify clean and hold every put the writer reported.
    #[test]
    fn a_copy_taken_at_any_instant_of_a_write_load_recovers_with_every_reported_commit() {
        let scratch = Scratch::new("snapshots");
        drop(seeded(&scratch.root()));
        let log = scratch.folder.join("committed.log");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "snapshots::crash_recovery_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, scratch.root())
            .env(LOG, &log)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut copies = Vec::new();
        for snapshot in 0..SNAPSHOTS {
            wait_for_lines(&log, (snapshot + 1) * 3);
            std::thread::sleep(Duration::from_millis(
                u64::try_from(snapshot * 7 % 11).unwrap(),
            ));
            signal(child.id(), "STOP");
            let copy = scratch.folder.join(format!("copy-{snapshot}"));
            fs::create_dir(&copy).unwrap();
            copy_tree(&scratch.root(), &copy);
            let committed = fs::read_to_string(&log).unwrap();
            signal(child.id(), "CONT");
            copies.push((copy.join("db"), committed));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        for (copy, committed) in copies {
            let report = verify(&copy);
            assert!(
                report.ok,
                "copy {} did not verify: {report:?}",
                copy.display()
            );
            let collection = NativeRoot::open(&copy).unwrap().collection(ITEMS).unwrap();
            for identifier in committed.lines() {
                collection
                    .read_document(identifier)
                    .unwrap_or_else(|error| {
                        panic!("{identifier} lost in {}: {error}", copy.display())
                    });
            }
        }
    }
}
