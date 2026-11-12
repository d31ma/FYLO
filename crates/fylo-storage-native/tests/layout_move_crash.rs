//! A layout move that a crash stops at any step leaves every document as it
//! was: before the marker the collection keeps its old layout, and from the
//! marker on the next writer finishes the move (ADR 0015 section 9).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use fylo_storage_native::{CollectionKind, CollectionLayout, NativeRoot, NativeWriteRoot};
use serde_json::{Value, json};

const CHILD: &str = "FYLO_LAYOUT_CRASH_CHILD";
const ROOT: &str = "FYLO_LAYOUT_CRASH_ROOT";
const ITEMS: &str = "items";
const FAILPOINTS: [&str; 3] = [
    "after-layout-staging",
    "after-layout-marker",
    "after-layout-swap",
];

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-layout-crash-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self {
            root: fs::canonicalize(path).unwrap(),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 30_000 + number)
}

fn put(writer: &NativeWriteRoot, number: usize) {
    let body = json!({ "name": format!("item{number}") }).to_string();
    writer
        .put_document(
            ITEMS,
            &identifier(number),
            body.as_bytes(),
            fylo_storage_native::PutDocumentOptions::default(),
        )
        .unwrap();
}

fn layout_of(name: &str) -> CollectionLayout {
    match name {
        "segments" => CollectionLayout::Segments,
        "files" => CollectionLayout::Files,
        other => panic!("unknown layout {other}"),
    }
}

/// Every live body and every deleted identifier of the collection.
fn contents(root: &Path) -> (BTreeMap<String, Value>, Vec<String>) {
    let collection = NativeRoot::open(root).unwrap().collection(ITEMS).unwrap();
    let bodies = collection
        .document_ids()
        .unwrap()
        .into_iter()
        .map(|id| {
            let bytes = collection.read_document(&id).unwrap().bytes;
            (id, serde_json::from_slice(&bytes).unwrap())
        })
        .collect();
    (bodies, collection.deleted_document_ids().unwrap())
}

fn layout(root: &Path) -> CollectionLayout {
    NativeRoot::open(root)
        .unwrap()
        .collection(ITEMS)
        .unwrap()
        .layout()
}

/// A collection that has been in segments once, written to there, and moved
/// back to files: the shape whose second move to segments once failed.
fn moved_once(root: &Path) -> NativeWriteRoot {
    let writer = NativeWriteRoot::open(root).unwrap();
    writer
        .create_collection(ITEMS, CollectionKind::Document, None)
        .unwrap();
    put(&writer, 0);
    writer
        .migrate_layout(ITEMS, CollectionLayout::Segments)
        .unwrap();
    for number in 1..5 {
        put(&writer, number);
    }
    writer.delete_document(ITEMS, &identifier(2)).unwrap();
    writer
        .migrate_layout(ITEMS, CollectionLayout::Files)
        .unwrap();
    put(&writer, 5);
    writer
}

fn run_crash_child(root: &Path, to: &str, failpoint: &str) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, to)
        .env(ROOT, root)
        .env("FYLO_RUST_FAILPOINT", failpoint)
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "move to {to} did not stop at {failpoint}"
    );
}

/// The other half of [`run_crash_child`]: does nothing in an ordinary run.
#[test]
fn crash_child() {
    let Ok(to) = std::env::var(CHILD) else {
        return;
    };
    let root = PathBuf::from(std::env::var(ROOT).unwrap());
    NativeWriteRoot::open(&root)
        .unwrap()
        .migrate_layout(ITEMS, layout_of(&to))
        .unwrap();
}

fn crash_then_finish(from: CollectionLayout, to: &str, failpoint: &str) {
    let scratch = Scratch::new(failpoint);
    let writer = moved_once(&scratch.root);
    if from == CollectionLayout::Segments {
        writer
            .migrate_layout(ITEMS, CollectionLayout::Segments)
            .unwrap();
    }
    NativeWriteRoot::close_logs_under(writer.path()).unwrap();
    drop(writer);
    let before = contents(&scratch.root);
    let step = format!("move to {to} stopped at {failpoint}");

    run_crash_child(&scratch.root, to, failpoint);

    let writer = NativeWriteRoot::open(&scratch.root).unwrap();
    writer.recover_collection(ITEMS).unwrap();
    let expected = if failpoint == "after-layout-staging" {
        from
    } else {
        layout_of(to)
    };
    assert_eq!(layout(&scratch.root), expected, "{step}");
    assert_eq!(contents(&scratch.root), before, "{step}");
    assert!(
        !scratch
            .root
            .join(".collections/items/.layout-staging")
            .exists(),
        "{step}: staging left behind"
    );

    writer.migrate_layout(ITEMS, layout_of(to)).unwrap();
    assert_eq!(contents(&scratch.root), before, "{step}: move run again");
    put(&writer, 9);
    writer.rebuild_collection(ITEMS).unwrap();
    assert_eq!(
        contents(&scratch.root).0.len(),
        before.0.len() + 1,
        "{step}"
    );
}

#[test]
fn a_second_move_to_segments_survives_a_crash_at_every_step() {
    for failpoint in FAILPOINTS {
        crash_then_finish(CollectionLayout::Files, "segments", failpoint);
    }
}

#[test]
fn a_move_back_to_files_survives_a_crash_at_every_step() {
    for failpoint in FAILPOINTS {
        crash_then_finish(CollectionLayout::Segments, "files", failpoint);
    }
}
