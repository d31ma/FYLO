//! A segment collection dropped and created again at the same path is a new
//! collection: no process that read the old one may read the old location
//! table for it, whether it dropped the collection itself or another
//! process did.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use fylo_format::Crc32c;
use fylo_storage_native::{
    CollectionKind, CollectionLayout, NativeRoot, NativeWriteRoot, PutDocumentOptions, RootConfig,
};
use serde_json::json;

const CHILD: &str = "FYLO_RECREATE_CACHE_CHILD";
const ROOT: &str = "FYLO_RECREATE_CACHE_ROOT";
const ITEMS: &str = "items";
const RESULT: &str = "RESULT ";
const CACHED: &str = "CACHED";
const OLD_ID: &str = "4VRNF540000";
const NEW_ID: &str = "4VRNF540001";
const SLOT_BYTES: usize = 64;
const SLOT_MAGIC: &[u8] = b"FYSL";
const SLOT_EPOCH_AT: usize = 56;
const SLOT_CHECKSUM_AT: usize = 60;

#[derive(Clone, Copy, Debug)]
enum Medium {
    Directory,
    Container,
    Database,
    Moved,
}

impl Medium {
    fn parse(name: &str) -> Self {
        match name {
            "directory" => Self::Directory,
            "container" => Self::Container,
            "database" => Self::Database,
            "moved" => Self::Moved,
            other => panic!("unknown medium {other}"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::Container => "container",
            Self::Database => "database",
            Self::Moved => "moved",
        }
    }
}

struct Scratch {
    directory: PathBuf,
    medium: Medium,
}

impl Scratch {
    fn new(label: &str, medium: Medium) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-recreate-cache-{label}-{}-{}-{}",
            medium.name(),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        let scratch = Self {
            directory: fs::canonicalize(path).unwrap(),
            medium,
        };
        if let Medium::Database = medium {
            NativeRoot::create_database(scratch.root()).unwrap();
        }
        scratch
    }

    fn root(&self) -> PathBuf {
        match self.medium {
            Medium::Database => self.directory.join("db.vdbx"),
            _ => self.directory.clone(),
        }
    }

    fn writer(&self) -> NativeWriteRoot {
        writer_of(&self.root(), self.medium)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Medium::Database = self.medium {
            let _ = NativeRoot::close_database(self.root());
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn writer_of(root: &Path, medium: Medium) -> NativeWriteRoot {
    let writer = NativeWriteRoot::open(root).unwrap();
    match medium {
        Medium::Database | Medium::Moved => writer,
        Medium::Directory => writer.with_config(segments()),
        Medium::Container => writer.with_config(segments()).with_segment_container(),
    }
}

fn segments() -> RootConfig {
    RootConfig {
        layout: CollectionLayout::Segments,
        ..RootConfig::default()
    }
}

/// Put one document whose record and location row are as long as any
/// other this file puts, so the old and the new collection reach the same
/// length of their location logs.
fn put(writer: &NativeWriteRoot, identifier: &str, name: &str) {
    let body = json!({ "name": name }).to_string();
    writer
        .put_document(
            ITEMS,
            identifier,
            body.as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

/// Create the collection holding one document. A moved collection is
/// created in the file layout and moved to segments, so the old and the new
/// collection reach segments at the same generation.
fn create_with(writer: &NativeWriteRoot, medium: Medium, (identifier, name): (&str, &str)) {
    writer
        .create_collection(ITEMS, CollectionKind::Document, None)
        .unwrap();
    put(writer, identifier, name);
    if let Medium::Moved = medium {
        writer
            .migrate_layout(ITEMS, CollectionLayout::Segments)
            .unwrap();
    }
}

/// Every live body of the collection by identifier, or the error a read
/// gave, as text so a child process can print it.
fn contents(root: &Path) -> String {
    let collection = NativeRoot::open(root).unwrap().collection(ITEMS).unwrap();
    let result = collection.document_ids().and_then(|ids| {
        ids.into_iter()
            .map(|id| {
                let bytes = collection.read_document(&id)?.bytes;
                Ok((id, String::from_utf8(bytes).unwrap()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
    });
    match result {
        Ok(bodies) => format!("{bodies:?}"),
        Err(error) => format!("error: {error}"),
    }
}

fn expected_new() -> String {
    let mut bodies = BTreeMap::new();
    bodies.insert(NEW_ID.to_owned(), json!({ "name": "new" }).to_string());
    format!("{bodies:?}")
}

fn drop_and_recreate(writer: &NativeWriteRoot, medium: Medium) {
    writer.drop_collection(ITEMS).unwrap();
    create_with(writer, medium, (NEW_ID, "new"));
}

fn recreated_in_the_same_process(medium: Medium) {
    let scratch = Scratch::new("same", medium);
    let writer = scratch.writer();
    create_with(&writer, medium, (OLD_ID, "old"));
    assert!(contents(&scratch.root()).contains(OLD_ID));

    drop_and_recreate(&writer, medium);

    assert_eq!(contents(&scratch.root()), expected_new());
}

fn recreated_by_another_process(medium: Medium) {
    reread_by_another_process(medium, |scratch| {
        drop_and_recreate(&scratch.writer(), medium);
    });
}

/// Re-create the collection, then give its slots the table epoch the old
/// collection had when the reader cached its table: the case a random
/// epoch only made unlikely. The layout id (ADR 0027) still tells the two
/// collections apart.
fn recreated_with_the_old_epoch_by_another_process(medium: Medium) {
    reread_by_another_process(medium, |scratch| {
        let epoch = newest_epoch(&slot_file(&scratch.root()));
        drop_and_recreate(&scratch.writer(), medium);
        force_epoch(&slot_file(&scratch.root()), epoch);
    });
}

/// A second process reads the collection, `recreate` replaces it, and the
/// second process reads again.
fn reread_by_another_process(medium: Medium, recreate: impl Fn(&Scratch)) {
    let scratch = Scratch::new("other", medium);
    let writer = scratch.writer();
    create_with(&writer, medium, (OLD_ID, "old"));
    drop(writer);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "reader_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, medium.name())
        .env(ROOT, scratch.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut next_result = move || {
        lines
            .by_ref()
            .map(Result::unwrap)
            .find_map(|line| line.split_once(RESULT).map(|(_, result)| result.to_owned()))
            .expect("the reader process printed no result")
    };
    assert!(next_result().contains(OLD_ID));
    assert_eq!(next_result(), CACHED);

    recreate(&scratch);
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "read").unwrap();
    drop(stdin);

    assert_eq!(next_result(), expected_new());
    assert!(child.wait().unwrap().success());
}

/// The reader half of [`recreated_by_another_process`]: reads the old
/// collection, so its location table is cached, waits until the parent has
/// dropped and re-created the collection, and reads again. Does nothing in
/// an ordinary run.
#[test]
fn reader_child() {
    let Ok(medium) = std::env::var(CHILD) else {
        return;
    };
    let root = PathBuf::from(std::env::var(ROOT).unwrap());
    Medium::parse(&medium);
    println!();
    println!("{RESULT}{}", contents(&root));
    println!("{RESULT}{CACHED}");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    println!("{RESULT}{}", contents(&root));
}

#[test]
fn directory_segments_recreated_in_the_same_process() {
    recreated_in_the_same_process(Medium::Directory);
}

#[test]
fn container_segments_recreated_in_the_same_process() {
    recreated_in_the_same_process(Medium::Container);
}

#[test]
fn database_collection_recreated_in_the_same_process() {
    recreated_in_the_same_process(Medium::Database);
}

#[test]
fn directory_segments_recreated_by_another_process() {
    recreated_by_another_process(Medium::Directory);
}

#[test]
fn container_segments_recreated_by_another_process() {
    recreated_by_another_process(Medium::Container);
}

#[test]
fn moved_segments_recreated_in_the_same_process() {
    recreated_in_the_same_process(Medium::Moved);
}

#[test]
fn moved_segments_recreated_by_another_process() {
    recreated_by_another_process(Medium::Moved);
}

#[test]
fn directory_segments_recreated_with_the_old_epoch_by_another_process() {
    recreated_with_the_old_epoch_by_another_process(Medium::Directory);
}

#[test]
fn moved_segments_recreated_with_the_old_epoch_by_another_process() {
    recreated_with_the_old_epoch_by_another_process(Medium::Moved);
}

/// A collection written before layout ids has a slot file of only its two
/// slots. It still reads and takes writes.
#[test]
fn directory_segments_without_a_layout_id_still_read_and_write() {
    let scratch = Scratch::new("no-layout-id", Medium::Directory);
    let writer = scratch.writer();
    create_with(&writer, Medium::Directory, (OLD_ID, "old"));
    let slots = slot_file(&scratch.root());
    let bytes = fs::read(&slots).unwrap();
    fs::write(&slots, &bytes[..2 * SLOT_BYTES]).unwrap();

    put(&scratch.writer(), NEW_ID, "new");

    let contents = contents(&scratch.root());
    assert!(contents.contains(OLD_ID), "{contents}");
    assert!(contents.contains(NEW_ID), "{contents}");
}

/// The generation slot file of the directory collection under `root`.
fn slot_file(root: &Path) -> PathBuf {
    root.join(".collections")
        .join(ITEMS)
        .join("segments")
        .join("state.slots")
}

fn slot_is_valid(slot: &[u8]) -> bool {
    let stored = u32::from_le_bytes(slot[SLOT_CHECKSUM_AT..].try_into().unwrap());
    slot[..4] == *SLOT_MAGIC && Crc32c::checksum(&slot[..SLOT_CHECKSUM_AT]) == stored
}

fn slot_field(slot: &[u8], index: usize) -> u64 {
    let start = 8 + index * 8;
    u64::from_le_bytes(slot[start..start + 8].try_into().unwrap())
}

/// The table epoch of the newest valid slot: the highest generation, then
/// the furthest end of the active segment, as readers order them.
fn newest_epoch(slots: &Path) -> u32 {
    let bytes = fs::read(slots).unwrap();
    let newest = bytes[..2 * SLOT_BYTES]
        .chunks(SLOT_BYTES)
        .filter(|slot| slot_is_valid(slot))
        .max_by_key(|slot| {
            (
                slot_field(slot, 0),
                slot_field(slot, 1),
                slot_field(slot, 2),
            )
        })
        .expect("the collection has a valid slot");
    u32::from_le_bytes(newest[SLOT_EPOCH_AT..SLOT_CHECKSUM_AT].try_into().unwrap())
}

/// Give every valid slot the table epoch `epoch`, with a new checksum.
fn force_epoch(slots: &Path, epoch: u32) {
    let mut bytes = fs::read(slots).unwrap();
    for slot in bytes[..2 * SLOT_BYTES].chunks_mut(SLOT_BYTES) {
        if slot_is_valid(slot) {
            slot[SLOT_EPOCH_AT..SLOT_CHECKSUM_AT].copy_from_slice(&epoch.to_le_bytes());
            let checksum = Crc32c::checksum(&slot[..SLOT_CHECKSUM_AT]);
            slot[SLOT_CHECKSUM_AT..].copy_from_slice(&checksum.to_le_bytes());
        }
    }
    fs::write(slots, bytes).unwrap();
}
