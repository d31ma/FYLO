//! `fylo rekey` stopped part way, and run while another process writes the
//! database (ADR 0016 section 9). The tests spawn this test binary again,
//! running [`a_rekey_child_process`] in the role an environment variable
//! names, so a failpoint stops one process only. Every key is generated
//! here.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::ContainerMedium;
use crate::{
    CollectionKind, ContainerCipher, ContainerFile, ContainerLeaseTiming, ContainerWriteOptions,
    DatabaseKey, DatabaseRekey, NativeRoot, NativeStorageErrorCode, NativeWriteRoot,
    PutDocumentOptions, RootOpenOptions,
};

const ROLE: &str = "FYLO_VDBX_REKEY_CHILD_ROLE";
const DATABASE: &str = "FYLO_VDBX_REKEY_CHILD_DATABASE";
const OLD_KEY: &str = "FYLO_VDBX_REKEY_CHILD_OLD_KEY";
const NEW_KEY: &str = "FYLO_VDBX_REKEY_CHILD_NEW_KEY";
const EXCLUSIVE: &str = "FYLO_VDBX_REKEY_CHILD_EXCLUSIVE";
const CHILD_TEST: &str = "segments::container_medium::tests::rekey::a_rekey_child_process";
const WAIT: Duration = Duration::from_mins(1);
/// Text in every document, which must never appear in an encrypted file.
const CANARY: &str = "BILBY-CANARY";

/// A generated secret, as hex, never a real one.
fn generated_secret() -> String {
    crate::hex_bytes(&ContainerCipher::new_salt().unwrap())
}

/// A directory holding `db.vdbx`, encrypted with a generated old key, and
/// the generated new key a rekey moves it to.
struct Rekeyed {
    directory: PathBuf,
    old: String,
    new: String,
}

impl Rekeyed {
    /// The database with `ledger`, 30 documents with one deleted, and
    /// `notes`, one document.
    fn create(label: &str) -> Self {
        let directory = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&directory).unwrap();
        let database = Self {
            directory: std::fs::canonicalize(directory).unwrap(),
            old: generated_secret(),
            new: generated_secret(),
        };
        NativeRoot::create_encrypted_database(database.path(), &Rekeyed::key(&database.old))
            .unwrap();
        let writer = database.writer(&database.old);
        for name in ["ledger", "notes"] {
            writer
                .create_collection(name, CollectionKind::Document, None)
                .unwrap();
        }
        for number in 0..30 {
            put(&writer, "ledger", number);
        }
        put(&writer, "notes", 1);
        writer
            .delete_document("ledger", &identifier(4), None)
            .unwrap();
        drop(writer);
        database.reopen();
        database
    }

    fn path(&self) -> PathBuf {
        self.directory.join("db.vdbx")
    }

    fn key(secret: &str) -> DatabaseKey {
        DatabaseKey::new(secret.as_bytes())
    }

    fn options(&self, secret: &str) -> RootOpenOptions {
        RootOpenOptions::new(self.path()).with_database_key(Self::key(secret))
    }

    fn writer(&self, secret: &str) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(self.options(secret)).unwrap()
    }

    /// Forget everything this process holds of the database, as a new
    /// process would start.
    fn reopen(&self) {
        NativeRoot::close_database(self.path()).unwrap();
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Require that the database opens with `secret` alone and holds what
    /// [`Self::create`] wrote, that it verifies, and that it shows none of
    /// it in the clear.
    fn require_whole(&self, secret: &str) {
        self.reopen();
        let other = if secret == self.old {
            &self.new
        } else {
            &self.old
        };
        let refused = NativeRoot::open_profiled(self.options(other)).unwrap_err();
        assert_eq!(refused.code(), NativeStorageErrorCode::KeyRequired);
        let root = NativeRoot::open_profiled(self.options(secret)).unwrap();
        let ledger = root.collection("ledger").unwrap();
        assert_eq!(ledger.document_ids().unwrap().len(), 29);
        assert_eq!(ledger.deleted_document_ids().unwrap(), [identifier(4)]);
        for number in (0..30).filter(|number| *number != 4) {
            let bytes = ledger.read_document(&identifier(number)).unwrap().bytes;
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap(),
                body(number)
            );
        }
        let notes = root.collection("notes").unwrap();
        assert!(notes.read_document(&identifier(1)).is_ok());
        let mut file = ContainerFile::open_reader(&self.path()).unwrap();
        file.unlock(secret.as_bytes()).unwrap();
        let verified = file.verify().unwrap();
        assert!(verified.ok && verified.problems.is_empty(), "{verified:?}");
        let bytes = std::fs::read(self.path()).unwrap();
        assert!(
            !bytes
                .windows(CANARY.len())
                .any(|window| window == CANARY.as_bytes())
        );
    }
}

impl Drop for Rekeyed {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.path());
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 80_000 + number)
}

fn body(number: usize) -> Value {
    json!({ "number": number, "note": CANARY })
}

fn put(writer: &NativeWriteRoot, collection: &str, number: usize) {
    writer
        .put_document(
            collection,
            &identifier(number),
            body(number).to_string().as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

/// What a child process does, by role; nothing when it is not a child.
///
/// - `rekey`: move the database to the new key with a failpoint that stops
///   the process part way.
/// - `rekey-fails`: move it with a failpoint that fails the rekey, as a
///   full disk would, and require the error and that no staging file is
///   left.
/// - `rekey-busy`: move it while another process writes it, and require
///   that it is refused in time.
/// - `hold`: write a document, say so, and keep the database open to
///   write until the parent says `release`.
#[test]
fn a_rekey_child_process() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let path = PathBuf::from(std::env::var(DATABASE).unwrap());
    let directory = path.parent().unwrap().to_owned();
    ContainerMedium::set_write_options(ContainerWriteOptions {
        busy_timeout: Duration::from_millis(300),
        exclusive: std::env::var(EXCLUSIVE).is_ok(),
        lease_timing: ContainerLeaseTiming::default(),
    });
    let old = DatabaseKey::new(std::env::var(OLD_KEY).unwrap());
    let new = DatabaseKey::new(std::env::var(NEW_KEY).unwrap());
    let writer =
        NativeWriteRoot::open_profiled(RootOpenOptions::new(&path).with_database_key(old)).unwrap();
    match role.as_str() {
        "rekey" => {
            let outcome = writer.rekey_database(DatabaseRekey::Rotate(&new));
            panic!("the failpoint did not stop the process: {outcome:?}");
        }
        "rekey-fails" => {
            let error = writer
                .rekey_database(DatabaseRekey::Rotate(&new))
                .unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains("failpoint") || message.contains("space"),
                "{error}"
            );
            assert!(!directory.join(".db.vdbx.pack-staging").exists());
        }
        "rekey-busy" => {
            let started = Instant::now();
            let error = writer
                .rekey_database(DatabaseRekey::Rotate(&new))
                .unwrap_err();
            assert_eq!(error.code(), NativeStorageErrorCode::RootLocked, "{error}");
            assert!(started.elapsed() >= Duration::from_millis(300));
            assert!(!directory.join(".db.vdbx.pack-staging").exists());
        }
        "hold" => {
            put(&writer, "notes", 2);
            std::fs::write(directory.join("hold.ready"), b"").unwrap();
            wait_until(&directory.join("release"), &mut []);
        }
        other => panic!("unknown child role {other}"),
    }
}

fn spawn(role: &str, database: &Rekeyed, environment: &[(&str, &str)]) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(DATABASE, database.path())
        .env(OLD_KEY, &database.old)
        .env(NEW_KEY, &database.new)
        .envs(environment.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn printed(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
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

#[test]
fn a_rekey_stopped_part_way_leaves_the_old_database_whole() {
    // After the staging file is built and flushed, just before the rename;
    // and in the middle of building it.
    for failpoint in ["after-pack-staging", "after-checkpoint-flush"] {
        let database = Rekeyed::create("vdbx-rekey-stopped");
        let output = spawn(
            "rekey",
            &database,
            &[
                ("FYLO_RUST_FAILPOINT", failpoint),
                ("FYLO_RUST_FAILPOINT_ACTION", "abort"),
            ],
        )
        .wait_with_output()
        .unwrap();
        assert!(
            !output.status.success() && !printed(&output).contains("did not stop"),
            "child rekey at {failpoint}:\n{}",
            printed(&output)
        );
        // The path still names the old file, which the old key opens.
        database.require_whole(&database.old.clone());
        if failpoint == "after-pack-staging" {
            assert!(
                database.directory.join(".db.vdbx.pack-staging").is_file(),
                "{:?}",
                database.names()
            );
        }
        // It takes writes as before, and the next rekey removes the staging
        // file and commits.
        put(&database.writer(&database.old), "notes", 3);
        database.reopen();
        database
            .writer(&database.old)
            .rekey_database(DatabaseRekey::Rotate(&Rekeyed::key(&database.new)))
            .unwrap();
        assert_eq!(database.names(), ["db.vdbx"], "{failpoint}");
        database.require_whole(&database.new.clone());
        let notes = NativeRoot::open_profiled(database.options(&database.new))
            .unwrap()
            .collection("notes")
            .unwrap();
        assert!(notes.read_document(&identifier(3)).is_ok());
    }
}

#[test]
fn a_rekey_that_fails_part_way_removes_what_it_wrote() {
    for action in ["enospc", "error"] {
        let database = Rekeyed::create("vdbx-rekey-fails");
        let output = spawn(
            "rekey-fails",
            &database,
            &[
                ("FYLO_RUST_FAILPOINT", "after-pack-staging"),
                ("FYLO_RUST_FAILPOINT_ACTION", action),
            ],
        )
        .wait_with_output()
        .unwrap();
        assert!(output.status.success(), "{action}:\n{}", printed(&output));
        assert_eq!(database.names(), ["db.vdbx"]);
        // The documents are as they were; only the recovery that settles
        // each collection before the copy wrote the database.
        database.require_whole(&database.old.clone());
    }
}

#[test]
fn a_rekey_waits_for_other_writer_processes_and_is_refused_in_time() {
    for exclusive in [false, true] {
        let database = Rekeyed::create("vdbx-rekey-busy");
        let lease: &[(&str, &str)] = if exclusive { &[(EXCLUSIVE, "1")] } else { &[] };
        let mut holder = spawn("hold", &database, lease);
        wait_until(&database.directory.join("hold.ready"), &mut [&mut holder]);
        let busy = spawn("rekey-busy", &database, lease)
            .wait_with_output()
            .unwrap();
        assert!(busy.status.success(), "{}", printed(&busy));
        std::fs::write(database.directory.join("release"), b"").unwrap();
        let held = holder.wait_with_output().unwrap();
        assert!(held.status.success(), "{}", printed(&held));
        for name in ["hold.ready", "release"] {
            std::fs::remove_file(database.directory.join(name)).unwrap();
        }
        // The holder's write is kept, under the old key, and the rekey runs
        // once the holder is gone.
        database.reopen();
        database
            .writer(&database.old)
            .rekey_database(DatabaseRekey::Rotate(&Rekeyed::key(&database.new)))
            .unwrap();
        database.require_whole(&database.new.clone());
        let notes = NativeRoot::open_profiled(database.options(&database.new))
            .unwrap()
            .collection("notes")
            .unwrap();
        assert!(notes.read_document(&identifier(2)).is_ok(), "{exclusive}");
    }
}
