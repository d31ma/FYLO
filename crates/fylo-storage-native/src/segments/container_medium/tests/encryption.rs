//! An encrypted `.vdbx` database root (ADR 0016 section 9): records sealed
//! with the file key and bound to their collection and LSN, the database
//! locked without its key, a wrong key refused at open, dictionaries and
//! snapshots sealed, and recovery after a crash. Every key is generated
//! here. The crash tests spawn this test binary again with a failpoint,
//! running [`an_encryption_child_process`].

use std::path::PathBuf;
use std::process::{Command, Stdio};

use fylo_format::{ContainerExtentHeader, ContainerExtentKind, LogScan};
use serde_json::{Value, json};

use super::ContainerMedium;
use crate::segments::SegmentMedium;
use crate::{
    CollectionKind, ContainerCipher, ContainerFile, DatabaseKey, NativeRoot,
    NativeStorageErrorCode, NativeWriteRoot, PutDocumentOptions, RootOpenOptions,
};

const ROLE: &str = "FYLO_VDBX_ENCRYPTION_CHILD_ROLE";
const DATABASE: &str = "FYLO_VDBX_ENCRYPTION_CHILD_DATABASE";
const KEY: &str = "FYLO_VDBX_ENCRYPTION_CHILD_KEY";
const CHILD_TEST: &str =
    "segments::container_medium::tests::encryption::an_encryption_child_process";
/// Text in every document, which must never appear in an encrypted file.
const CANARY: &str = "QUOKKA-CANARY";
/// A collection name, which the sealed directory must hide too.
const LEDGER: &str = "quokka-ledger";

/// A generated secret, as hex, never a real one.
fn generated_secret() -> String {
    crate::hex_bytes(&ContainerCipher::new_salt().unwrap())
}

/// A directory holding `db.vdbx`, encrypted with a generated key.
struct Encrypted {
    directory: PathBuf,
    secret: String,
}

impl Encrypted {
    fn create(label: &str) -> Self {
        Self::create_with(label, true)
    }

    /// The same database, plain, to show what encryption hides.
    fn create_with(label: &str, encrypted: bool) -> Self {
        let directory = std::env::temp_dir().join(crate::write::unique_name(label));
        std::fs::create_dir(&directory).unwrap();
        let database = Self {
            directory: std::fs::canonicalize(directory).unwrap(),
            secret: generated_secret(),
        };
        if encrypted {
            NativeRoot::create_encrypted_database(database.path(), &database.key()).unwrap();
        } else {
            NativeRoot::create_database(database.path()).unwrap();
        }
        database
    }

    fn path(&self) -> PathBuf {
        self.directory.join("db.vdbx")
    }

    fn key(&self) -> DatabaseKey {
        DatabaseKey::new(self.secret.as_bytes())
    }

    fn options(&self) -> RootOpenOptions {
        RootOpenOptions::new(self.path()).with_database_key(self.key())
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open_profiled(self.options()).unwrap()
    }

    fn root(&self) -> NativeRoot {
        NativeRoot::open_profiled(self.options()).unwrap()
    }

    /// Forget everything this process holds of the database, as a new
    /// process would start.
    fn reopen(&self) {
        NativeRoot::close_database(self.path()).unwrap();
    }

    fn bytes(&self) -> Vec<u8> {
        std::fs::read(self.path()).unwrap()
    }

    fn contains(&self, text: &str) -> bool {
        self.bytes()
            .windows(text.len())
            .any(|window| window == text.as_bytes())
    }

    /// The medium of `collection`, with the key.
    fn medium(&self, collection: &str) -> ContainerMedium {
        let root = self.root();
        let block = root
            .collection(collection)
            .unwrap()
            .database_block()
            .unwrap();
        root.database().unwrap().medium(block).unwrap()
    }

    /// The file as a reader opened now sees it, unlocked.
    fn unlocked_file(&self) -> ContainerFile {
        let mut file = ContainerFile::open_reader(&self.path()).unwrap();
        file.unlock(self.secret.as_bytes()).unwrap();
        file
    }
}

impl Drop for Encrypted {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.path());
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 60_000 + number)
}

fn order(number: usize) -> Value {
    const CITIES: [&str; 5] = ["Halifax", "Saskatoon", "Kelowna", "Moncton", "Sherbrooke"];
    const ITEMS: [&str; 4] = [
        "Mechanical keyboard, brown switches",
        "Wireless mouse with USB-C receiver",
        "27 inch monitor, 1440p, matte panel",
        "Braided charging cable, two metres",
    ];
    json!({
        "customer": format!("customer-{number:05} {CANARY}"),
        "city": CITIES[number % CITIES.len()],
        "lines": (0..(number % 4 + 2))
            .map(|line| format!(
                "SKU-{:03} x{} {}",
                (number * 7 + line) % 250,
                line + 1,
                ITEMS[(number + line) % ITEMS.len()]
            ))
            .collect::<Vec<_>>(),
        "notes": "Leave the parcel at the side door if nobody answers the bell.",
    })
}

/// An order with its history, about 4 KiB, so a few hundred reach 1 MiB.
fn large_order(number: usize) -> Value {
    let mut order = order(number);
    order["history"] = (0..24)
        .map(|step| {
            format!(
                "step {step}: parcel {number:05} scanned at depot {} by courier {}, {}",
                (number + step) % 17,
                (number * 3 + step) % 29,
                [
                    "on time",
                    "delayed by weather",
                    "rerouted",
                    "held for pickup"
                ][step % 4]
            )
        })
        .collect::<Vec<_>>()
        .into();
    order
}

fn put(writer: &NativeWriteRoot, collection: &str, number: usize, body: &Value) {
    writer
        .put_document(
            collection,
            &identifier(number),
            body.to_string().as_bytes(),
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

fn write_orders(database: &Encrypted) {
    let writer = database.writer();
    for name in [LEDGER, "notes"] {
        writer
            .create_collection(name, CollectionKind::Document, None)
            .unwrap();
    }
    for number in 0..40 {
        put(&writer, LEDGER, number, &order(number));
    }
    put(&writer, "notes", 1, &json!({ "text": CANARY }));
    writer
        .patch_document(LEDGER, &identifier(3), br#"{"city":"Moose Jaw"}"#, None)
        .unwrap();
    writer
        .delete_document(LEDGER, &identifier(4), None)
        .unwrap();
    // A checkpoint seals the location and index snapshots too.
    writer.rebuild_collection(LEDGER).unwrap();
    put(&writer, LEDGER, 40, &order(40));
}

#[test]
fn an_encrypted_database_round_trips_with_its_key_and_hides_its_documents() {
    let database = Encrypted::create("vdbx-encrypted-round-trip");
    write_orders(&database);
    let check = |root: &NativeRoot| {
        assert_eq!(root.database_encryption(), Some((true, false)));
        assert_eq!(
            root.database_collection_names().unwrap(),
            vec![LEDGER.to_owned(), "notes".to_owned()]
        );
        let ledger = root.collection(LEDGER).unwrap();
        assert_eq!(ledger.document_ids().unwrap().len(), 40);
        assert_eq!(ledger.deleted_document_ids().unwrap(), vec![identifier(4)]);
        assert_eq!(read(root, LEDGER, 7), order(7));
        assert_eq!(read(root, LEDGER, 40), order(40));
        assert_eq!(read(root, LEDGER, 3)["city"], "Moose Jaw");
        assert_eq!(read(root, "notes", 1)["text"], CANARY);
    };
    check(&database.root());
    database.reopen();
    check(&database.root());

    // Neither a document, a collection's name nor an index key is in the
    // file's bytes; a plain database written the same way shows them.
    for text in [CANARY, LEDGER, "Halifax", "customer"] {
        assert!(!database.contains(text), "{text} is in the encrypted file");
    }
    let plain = Encrypted::create_with("vdbx-encrypted-control", false);
    write_orders(&plain);
    for text in [CANARY, LEDGER] {
        assert!(plain.contains(text), "{text} is not in the plain file");
    }

    // `fylo verify` needs no key, and `fylo inspect` says what it can.
    let reader = ContainerFile::open_reader(&database.path()).unwrap();
    let verified = reader.verify().unwrap();
    assert!(verified.ok && verified.problems.is_empty(), "{verified:?}");
    let report = reader.report().unwrap();
    assert!(report.encrypted && report.locked && report.collections.is_empty());
    let report = database.unlocked_file().report().unwrap();
    assert!(report.encrypted && !report.locked);
    assert_eq!(report.collections.len(), 2);
    let report = ContainerFile::open_reader(&plain.path())
        .unwrap()
        .report()
        .unwrap();
    assert!(!report.encrypted && !report.locked);
}

#[test]
fn without_its_key_an_encrypted_database_is_locked_and_refuses_every_write() {
    let database = Encrypted::create("vdbx-encrypted-locked");
    write_orders(&database);
    // A handle with the key, open in this process, unlocks nothing for one
    // without it.
    let unlocked = database.root();
    let before = database.bytes();

    let locked = NativeRoot::open(database.path()).unwrap();
    assert_eq!(locked.database_encryption(), Some((true, true)));
    assert_eq!(
        locked.database_collection_names().unwrap(),
        Vec::<String>::new()
    );
    let refused = locked.collection(LEDGER).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::KeyRequired);
    assert_eq!(refused.code().as_str(), "EKEY");
    assert!(
        refused.to_string().contains("fylo.encryption.key"),
        "{refused}"
    );

    let writer = NativeWriteRoot::open(database.path()).unwrap();
    let body = order(99).to_string();
    let refusals: [(&str, Result<(), crate::NativeStorageError>); 5] = [
        (
            "put",
            writer.put_document(
                LEDGER,
                &identifier(99),
                body.as_bytes(),
                PutDocumentOptions::default(),
            ),
        ),
        (
            "put into a new collection",
            writer.put_document(
                "fresh",
                &identifier(99),
                body.as_bytes(),
                PutDocumentOptions::default(),
            ),
        ),
        (
            "create",
            writer
                .create_collection("fresh", CollectionKind::Document, None)
                .map(drop),
        ),
        ("drop", writer.drop_collection("notes")),
        (
            "delete",
            writer
                .delete_document(LEDGER, &identifier(1), None)
                .map(drop),
        ),
    ];
    for (write, outcome) in refusals {
        assert_eq!(
            outcome.unwrap_err().code(),
            NativeStorageErrorCode::KeyRequired,
            "{write}"
        );
    }
    assert!(
        database.bytes() == before,
        "a locked writer changed the file"
    );
    assert_eq!(read(&unlocked, LEDGER, 7), order(7));

    // A wrong key is refused at open, as EKEY rather than damage.
    let wrong = RootOpenOptions::new(database.path())
        .with_database_key(DatabaseKey::new(generated_secret().as_bytes()));
    let refused = NativeRoot::open_profiled(wrong.clone()).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::KeyRequired);
    assert!(refused.to_string().contains("does not match"), "{refused}");
    database.reopen();
    assert_eq!(
        NativeWriteRoot::open_profiled(wrong).unwrap_err().code(),
        NativeStorageErrorCode::KeyRequired
    );
    // A key given to a plain database is not needed, and changes nothing.
    let plain = Encrypted::create_with("vdbx-encrypted-plain-key", false);
    let root = NativeRoot::open_profiled(plain.options()).unwrap();
    assert_eq!(root.database_encryption(), Some((false, false)));
}

/// Where record `number` of `collection` sits in the file: its byte
/// offset and its frame's length.
fn frame_at(database: &Encrypted, collection: &str, lsn: u64) -> (u64, usize) {
    let medium = database.medium(collection);
    let extent_bytes = ContainerFile::open_reader(&database.path())
        .unwrap()
        .superblock()
        .extent_bytes();
    for number in medium.segment_numbers().unwrap() {
        let bytes = medium.require_segment(number).unwrap();
        let scan = LogScan::segment_with(&bytes, &*medium.frame_bodies()).unwrap();
        let ends: Vec<u64> = scan
            .records
            .iter()
            .skip(1)
            .map(|(offset, _)| *offset)
            .chain([scan.valid_length])
            .collect();
        for ((offset, record), end) in scan.records.iter().zip(ends) {
            if record.lsn == lsn {
                let extent = medium.extent_of(number);
                let at = extent * extent_bytes + ContainerExtentHeader::PAYLOAD_OFFSET + offset;
                return (at, usize::try_from(end - offset).unwrap());
            }
        }
    }
    panic!("collection {collection} has no record {lsn}");
}

/// The LSN of the record that holds the newest version of document
/// `number`.
fn lsn_of(database: &Encrypted, collection: &str, number: usize) -> u64 {
    let medium = database.medium(collection);
    let id = identifier(number);
    let mut newest = None;
    for segment in medium.segment_numbers().unwrap() {
        let bytes = medium.require_segment(segment).unwrap();
        let scan = LogScan::segment_with(&bytes, &*medium.frame_bodies()).unwrap();
        for (_, record) in scan.records {
            if String::from_utf8_lossy(&record.manifest).contains(&id) {
                newest = Some(record.lsn);
            }
        }
    }
    newest.unwrap()
}

fn copy_frame(database: &Encrypted, from: (u64, usize), to: (u64, usize)) {
    use std::io::{Read, Seek, SeekFrom, Write};
    assert_eq!(from.1, to.1, "the frames must be the same length");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(database.path())
        .unwrap();
    let mut frame = vec![0_u8; from.1];
    file.seek(SeekFrom::Start(from.0)).unwrap();
    file.read_exact(&mut frame).unwrap();
    file.seek(SeekFrom::Start(to.0)).unwrap();
    file.write_all(&frame).unwrap();
    file.sync_all().unwrap();
}

/// A document of fixed length, so the records holding it are as long as
/// one another.
fn fixed(text: &str) -> Value {
    json!({ "text": text, "padding": fylo_test_support::incompressible_text(7, 2048) })
}

/// A database whose collections `east` and `west` hold document 1, the
/// same in each, and `east` documents 2 to 6 with the same body, so some
/// of their records are as long as document 1's.
fn swappable(label: &str) -> Encrypted {
    let database = Encrypted::create(label);
    let writer = database.writer();
    for name in ["east", "west"] {
        writer
            .create_collection(name, CollectionKind::Document, None)
            .unwrap();
        put(&writer, name, 1, &fixed("first"));
    }
    for number in 2..=6 {
        put(&writer, "east", number, &fixed("first"));
    }
    database
}

fn read_error(database: &Encrypted, collection: &str, number: usize) -> crate::NativeStorageError {
    database.reopen();
    database
        .root()
        .collection(collection)
        .unwrap()
        .read_document(&identifier(number))
        .unwrap_err()
}

#[test]
fn a_frame_moved_into_another_collection_is_refused() {
    let database = swappable("vdbx-encrypted-swap");
    let east = frame_at(&database, "east", lsn_of(&database, "east", 1));
    let west = frame_at(&database, "west", lsn_of(&database, "west", 1));
    // The same document, written the same way, in another collection.
    copy_frame(&database, west, east);
    let error = read_error(&database, "east", 1);
    assert_eq!(
        error.code(),
        NativeStorageErrorCode::CorruptDocument,
        "{error}"
    );
    assert!(error.to_string().contains("EVDBX_CORRUPT"), "{error}");
    // The record it was copied from still reads where it belongs.
    assert_eq!(read(&database.root(), "west", 1), fixed("first"));
    // `fylo verify` checks frames by their checksum without the key, so a
    // whole frame moved in place passes it: only the key finds it.
    let verified = ContainerFile::open_reader(&database.path())
        .unwrap()
        .verify()
        .unwrap();
    assert!(verified.ok, "{verified:?}");
}

#[test]
fn a_replayed_frame_is_refused_rather_than_read_as_another_record() {
    let database = swappable("vdbx-encrypted-replay");
    let first = frame_at(&database, "east", lsn_of(&database, "east", 1));
    let (number, later) = (2..=6)
        .map(|number| {
            (
                number,
                frame_at(&database, "east", lsn_of(&database, "east", number)),
            )
        })
        .find(|(_, later)| later.1 == first.1)
        .expect("a later record as long as the first");
    // Document 1's record, replayed over a later one: it opens, since it
    // is a record of this collection, but it is not the record whose place
    // it took, and nothing it holds is served for the later document.
    copy_frame(&database, first, later);
    let error = read_error(&database, "east", number);
    assert_eq!(
        error.code(),
        NativeStorageErrorCode::CorruptDocument,
        "{error}"
    );
}

#[test]
fn an_older_version_replayed_over_the_newest_is_never_served() {
    let database = swappable("vdbx-encrypted-rollback");
    // Versions that differ in one character, until the newest record is as
    // long as an older one, which compression makes likely within a few.
    let padding = fylo_test_support::incompressible_text(7, 2048);
    let mut versions = Vec::new();
    let (older, newest) = (0..64)
        .find_map(|round| {
            let patch = json!({ "padding": format!("{round:02}{}", &padding[2..]) });
            database
                .writer()
                .patch_document("west", &identifier(1), patch.to_string().as_bytes(), None)
                .unwrap();
            let newest = frame_at(&database, "west", lsn_of(&database, "west", 1));
            let older = versions
                .iter()
                .copied()
                .find(|older: &(u64, usize)| older.1 == newest.1);
            versions.push(newest);
            older.map(|older| (older, newest))
        })
        .expect("an older version as long as the newest");
    // An older version of the same document, replayed over the newest: it
    // opens, and names the same document and entry, but its LSN is not the
    // one the newest location names.
    copy_frame(&database, older, newest);
    let error = read_error(&database, "west", 1);
    assert_eq!(
        error.code(),
        NativeStorageErrorCode::CorruptDocument,
        "{error}"
    );
}

#[test]
fn dictionaries_and_schema_snapshots_are_sealed_in_an_encrypted_database() {
    let database = Encrypted::create("vdbx-encrypted-dictionary");
    let writer = database.writer();
    writer
        .create_collection(LEDGER, CollectionKind::Document, None)
        .unwrap();
    let schema = |ordinal: u32| {
        format!("{{\"label\":\"v{ordinal} {CANARY}\",\"schema\":{{\"customer\":\"^.+$\"}}}}")
    };
    for ordinal in 1..=2 {
        writer
            .commit_schema_version(LEDGER, ordinal, schema(ordinal).as_bytes(), |_| Ok(()))
            .unwrap();
    }
    // Past 1 MiB of records, so the next checkpoint trains a dictionary.
    let number = 300;
    for number in 0..number {
        put(&writer, LEDGER, number, &large_order(number));
    }
    writer.rebuild_collection(LEDGER).unwrap();
    for later in number..number + 20 {
        put(&writer, LEDGER, later, &large_order(later));
    }
    drop(writer);

    let file = database.unlocked_file();
    let ledger = file
        .collections()
        .iter()
        .find(|held| held.header.name == LEDGER)
        .unwrap();
    let slot = file.read_checkpoint(ledger.block).unwrap().unwrap();
    let (extent, id, _) = slot.dictionary.expect("the checkpoint trains a dictionary");
    assert!(
        file.dictionary_header(ledger.header.id, extent)
            .unwrap()
            .sealed
    );
    assert_eq!(
        file.read_dictionary(ledger.header.id, extent).unwrap().id,
        id
    );
    assert!(slot.schemas.is_some() && slot.locations.is_some() && slot.index.is_some());
    let medium = database.medium(LEDGER);
    let frames = medium.frame_bodies();
    let mut named = 0;
    for segment in medium.segment_numbers().unwrap() {
        let bytes = medium.require_segment(segment).unwrap();
        for (_, stored) in LogScan::stored_bodies(&bytes, &*frames).unwrap().0 {
            let parsed = crate::ContainerStoredBody::parse(stored).unwrap();
            assert!(parsed.sealed);
            named += usize::from(parsed.dictionary_id == id);
        }
    }
    assert!(named >= 20, "later records use the dictionary: {named}");
    drop((medium, frames));

    database.reopen();
    let root = database.root();
    assert_eq!(root.schema_registry_head(LEDGER).unwrap(), Some(2));
    for number in [0, 123, number - 1, number + 19] {
        assert_eq!(read(&root, LEDGER, number), large_order(number), "{number}");
    }
    assert!(!database.contains(CANARY));
    let verified = ContainerFile::open_reader(&database.path())
        .unwrap()
        .verify()
        .unwrap();
    assert!(verified.ok && verified.problems.is_empty(), "{verified:?}");
    let file = database.unlocked_file();
    let ledger = file
        .collections()
        .iter()
        .find(|held| held.header.name == LEDGER)
        .unwrap();
    assert_eq!(
        file.allocator()
            .owned(ContainerExtentKind::Dictionary, ledger.header.id)
            .len(),
        1
    );
}

/// What a child process does, by role; nothing when it is not a child.
///
/// - `write`: put document 100 into the ledger with a failpoint that
///   stops the process, as a crash would.
/// - `rebuild`: checkpoint the ledger with a failpoint that stops the
///   process part way.
/// - `compact`: compact and shrink the database with a failpoint that
///   stops the process part way.
#[test]
fn an_encryption_child_process() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let options = RootOpenOptions::new(PathBuf::from(std::env::var(DATABASE).unwrap()))
        .with_database_key(DatabaseKey::new(std::env::var(KEY).unwrap()));
    let writer = NativeWriteRoot::open_profiled(options).unwrap();
    let outcome = match role.as_str() {
        "write" => writer.put_document(
            LEDGER,
            &identifier(100),
            order(100).to_string().as_bytes(),
            PutDocumentOptions::default(),
        ),
        "rebuild" => writer.rebuild_collection(LEDGER),
        "compact" => writer.compact_database(true).map(drop),
        other => panic!("unknown child role {other}"),
    };
    panic!("the failpoint did not stop the process: {outcome:?}");
}

/// Run [`an_encryption_child_process`] as `role` on `database`, stopped at
/// `failpoint`.
fn stopped(role: &str, database: &Encrypted, failpoint: &str) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(DATABASE, database.path())
        .env(KEY, &database.secret)
        .env("FYLO_RUST_FAILPOINT", failpoint)
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && !printed.contains("did not stop"),
        "child {role} at {failpoint}:\n{printed}"
    );
}

fn recovered(database: &Encrypted, written: &[usize]) {
    database.reopen();
    let writer = database.writer();
    writer.recover_collection(LEDGER).unwrap();
    put(&writer, LEDGER, 200, &order(200));
    drop(writer);
    database.reopen();
    let root = database.root();
    for number in written.iter().copied().chain([200]) {
        assert_eq!(read(&root, LEDGER, number), order(number), "{number}");
    }
    let verified = ContainerFile::open_reader(&database.path())
        .unwrap()
        .verify()
        .unwrap();
    assert!(verified.ok, "{verified:?}");
    assert!(!database.contains(CANARY));
}

#[test]
fn an_encrypted_database_recovers_after_a_crash() {
    for (role, failpoint) in [
        ("write", "after-log-append"),
        ("write", "after-log-sync"),
        ("rebuild", "after-checkpoint-flush"),
    ] {
        let database = Encrypted::create("vdbx-encrypted-crash");
        write_orders(&database);
        database.reopen();
        stopped(role, &database, failpoint);
        let mut written: Vec<usize> = (0..=40).filter(|number| *number != 4).collect();
        written.retain(|number| *number != 3);
        recovered(&database, &written);
        // Document 100 was appended before the stop: recovery keeps it
        // once it was synced and published, and rolls it back otherwise;
        // either way the ledger reads.
        let root = database.root();
        let ledger = root.collection(LEDGER).unwrap();
        if let Ok(document) = ledger.read_document(&identifier(100)) {
            assert_eq!(
                serde_json::from_slice::<Value>(&document.bytes).unwrap(),
                order(100)
            );
        }
    }
}

/// Orders written and deleted again, so a compaction has records to drop
/// and a shrink extents to give back.
fn churned(label: &str) -> Encrypted {
    let database = Encrypted::create(label);
    write_orders(&database);
    let writer = database.writer();
    for number in 41..200 {
        put(&writer, LEDGER, number, &large_order(number));
    }
    for number in 41..200 {
        writer
            .delete_document(LEDGER, &identifier(number), None)
            .unwrap();
    }
    database
}

/// Require that the ledger and the notes read as [`write_orders`] wrote
/// them, and that the file shows none of it.
fn compacted_reads(database: &Encrypted) {
    let root = database.root();
    let ledger = root.collection(LEDGER).unwrap();
    assert_eq!(ledger.document_ids().unwrap().len(), 40);
    for number in (0..=40).filter(|number| ![3, 4].contains(number)) {
        assert_eq!(read(&root, LEDGER, number), order(number), "{number}");
    }
    assert_eq!(read(&root, LEDGER, 3)["city"], "Moose Jaw");
    assert_eq!(read(&root, "notes", 1)["text"], CANARY);
    let verified = ContainerFile::open_reader(&database.path())
        .unwrap()
        .verify()
        .unwrap();
    assert!(verified.ok, "{verified:?}");
    for text in [CANARY, LEDGER, "Halifax", "depot"] {
        assert!(!database.contains(text), "{text} is in the encrypted file");
    }
}

#[test]
fn an_encrypted_database_compacts_shrinks_and_salvages_only_with_its_key() {
    let database = churned("vdbx-encrypted-compact");
    database.reopen();

    // Without the key, compaction, a shrink and a salvage are refused and
    // change nothing.
    let before = database.bytes();
    let locked = NativeWriteRoot::open(database.path()).unwrap();
    let out = database.directory.join("salvaged");
    let refusals = [
        locked.compact_database(false).map(drop),
        locked.compact_database(true).map(drop),
        locked.salvage_database(&out).map(drop),
    ];
    for outcome in refusals {
        assert_eq!(
            outcome.unwrap_err().code(),
            NativeStorageErrorCode::KeyRequired
        );
    }
    drop(locked);
    database.reopen();
    assert!(
        database.bytes() == before,
        "a locked writer changed the file"
    );
    assert!(!out.exists());

    // With it, the copied records are sealed again, each bound to its LSN.
    let report = database.writer().compact_database(false).unwrap();
    let (name, ledger) = &report.collections[0];
    assert_eq!(name, LEDGER);
    assert!(ledger.segments_compacted >= 1, "{report:?}");
    assert!(ledger.bytes_after < ledger.bytes_before, "{report:?}");
    compacted_reads(&database);
    let report = database.writer().compact_database(true).unwrap();
    assert!(
        report.file_bytes_after < report.file_bytes_before,
        "{report:?}"
    );
    compacted_reads(&database);
    database.reopen();
    compacted_reads(&database);
    put(&database.writer(), LEDGER, 300, &order(300));
    database.reopen();
    assert_eq!(read(&database.root(), LEDGER, 300), order(300));

    // A salvage with the key writes the documents out as a plain root.
    let report = database.writer().salvage_database(&out).unwrap();
    assert!(
        report
            .collections
            .iter()
            .all(|(_, salvaged)| salvaged.damaged.is_empty()),
        "{report:?}"
    );
    let salvaged = NativeRoot::open(&out).unwrap();
    assert_eq!(read(&salvaged, LEDGER, 7), order(7));
    assert_eq!(read(&salvaged, LEDGER, 300), order(300));
    assert_eq!(read(&salvaged, "notes", 1)["text"], CANARY);
}

#[test]
fn an_encrypted_database_recovers_after_a_crash_in_compaction() {
    for failpoint in ["after-compaction-record", "after-shrink-relocate"] {
        let database = churned("vdbx-encrypted-compact-crash");
        database.reopen();
        stopped("compact", &database, failpoint);
        database.reopen();
        compacted_reads(&database);
        database.writer().compact_database(true).unwrap();
        database.reopen();
        compacted_reads(&database);
    }
}
