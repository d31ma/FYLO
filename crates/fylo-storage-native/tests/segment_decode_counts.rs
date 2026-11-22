//! Reading documents out of a segment commit that holds many of them
//! decodes each commit's manifest once, and a point read decodes only its
//! own entry (FYLO-CR2). The counts come from `SegmentReadLog`.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fylo_storage_native::{
    CollectionKind, CollectionLayout, DocumentWrite, NativeCollection, NativeRoot, NativeWriteRoot,
    PlainDocumentCodec, RootConfig, SegmentReadLog,
};
use serde_json::json;

struct Fixture(PathBuf);

impl Fixture {
    const COLLECTION: &str = "items";
    const PER_COMMIT: usize = 200;
    const LARGE_NOTE_BYTES: usize = 6_000;

    fn new(label: &str) -> Self {
        let root = Self::folder(label);
        fs::create_dir(&root).unwrap();
        let fixture = Self(root);
        fixture
            .writer()
            .create_collection(Self::COLLECTION, CollectionKind::Document, None)
            .unwrap();
        fixture
    }

    fn folder(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        fylo_test_support::TestFolder::path().join(format!(
            "fylo-decode-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(&self.0)
            .unwrap()
            .with_config(RootConfig {
                layout: CollectionLayout::Segments,
                ..RootConfig::default()
            })
    }

    fn collection(&self) -> NativeCollection {
        NativeRoot::open(&self.0)
            .unwrap()
            .collection(Self::COLLECTION)
            .unwrap()
    }

    fn id(number: usize) -> String {
        format!("4VRNF5{number:05}")
    }

    fn body(number: usize, note_bytes: usize) -> serde_json::Map<String, serde_json::Value> {
        let note = "n".repeat(note_bytes);
        json!({ "number": number, "name": format!("user{number}"), "note": note })
            .as_object()
            .unwrap()
            .clone()
    }

    /// One commit holding `PER_COMMIT` new documents, numbered from `first`.
    fn commit(&self, first: usize) {
        self.commit_with_notes(first, 0);
    }

    /// One commit whose documents each carry a note of `note_bytes` bytes.
    fn commit_with_notes(&self, first: usize, note_bytes: usize) {
        let writes: Vec<DocumentWrite> = (first..first + Self::PER_COMMIT)
            .map(|number| DocumentWrite::Put {
                identifier: Self::id(number),
                fields: Self::body(number, note_bytes),
            })
            .collect();
        self.writer()
            .commit_document_writes_with(Self::COLLECTION, &writes, &PlainDocumentCodec)
            .unwrap();
    }

    fn ids(count: usize) -> Vec<String> {
        (0..count).map(Self::id).collect()
    }

    fn overwrite(&self, number: usize) {
        let body = json!({ "number": number, "name": "changed" }).to_string();
        self.writer()
            .patch_document(Self::COLLECTION, &Self::id(number), body.as_bytes())
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn counted<T>(step: impl FnOnce() -> T) -> (T, SegmentReadLog) {
    SegmentReadLog::start();
    let result = step();
    (result, SegmentReadLog::finish())
}

#[test]
fn a_point_read_decodes_only_its_own_entry() {
    let fixture = Fixture::new("point");
    fixture.commit(0);
    let collection = fixture.collection();
    collection.read_document(&Fixture::id(0)).unwrap();
    let (stored, log) = counted(|| collection.read_document(&Fixture::id(150)).unwrap());
    let body: serde_json::Value = serde_json::from_slice(&stored.bytes).unwrap();
    assert_eq!(body["number"], 150);
    assert_eq!(log.entries_decoded(), 1, "{log:?}");
    assert_eq!(log.frames_decoded(), 1, "{log:?}");
}

#[test]
fn a_batch_read_decodes_each_commit_once() {
    let fixture = Fixture::new("batch");
    fixture.commit(0);
    fixture.commit(Fixture::PER_COMMIT);
    let collection = fixture.collection();
    collection.read_document(&Fixture::id(0)).unwrap();
    let ids = Fixture::ids(2 * Fixture::PER_COMMIT);
    let (stored, log) = counted(|| collection.read_documents(&ids).unwrap());
    assert_eq!(stored.len(), 2 * Fixture::PER_COMMIT);
    assert_eq!(log.entries_decoded(), 2 * Fixture::PER_COMMIT, "{log:?}");
    assert_eq!(log.frames_decoded(), 2, "{log:?}");
}

#[test]
fn a_batch_read_decodes_a_commit_larger_than_one_read_span_once() {
    let fixture = Fixture::new("large");
    fixture.commit_with_notes(0, Fixture::LARGE_NOTE_BYTES);
    let collection = fixture.collection();
    collection.read_document(&Fixture::id(0)).unwrap();
    let ids = Fixture::ids(Fixture::PER_COMMIT);
    let (stored, log) = counted(|| collection.read_documents(&ids).unwrap());
    let bytes: usize = stored.iter().map(|document| document.bytes.len()).sum();
    assert!(bytes > 1024 * 1024, "the commit is only {bytes} bytes");
    assert_eq!(log.entries_decoded(), Fixture::PER_COMMIT, "{log:?}");
    assert_eq!(log.frames_decoded(), 1, "{log:?}");
}

#[test]
fn compaction_decodes_each_copied_commit_once() {
    let fixture = Fixture::new("compact");
    fixture.commit(0);
    fixture.overwrite(0);
    let writer = fixture.writer();
    let (report, log) = counted(|| writer.compact_collection(Fixture::COLLECTION).unwrap());
    println!("{report:?} {log:?}");
    assert!(
        report.documents_copied >= Fixture::PER_COMMIT - 1,
        "{report:?}"
    );
    assert!(
        log.entries_decoded() <= 4 * (Fixture::PER_COMMIT + 1),
        "{log:?}"
    );
}

#[test]
fn salvage_decodes_each_commit_once() {
    let fixture = Fixture::new("salvage");
    fixture.commit(0);
    let out = Fixture::folder("salvage-out");
    let writer = fixture.writer();
    let (report, log) = counted(|| {
        writer
            .salvage_collection(Fixture::COLLECTION, &out)
            .unwrap()
    });
    let _ = fs::remove_dir_all(&out);
    println!("{report:?} {log:?}");
    assert_eq!(report.recovered, Fixture::PER_COMMIT, "{report:?}");
    assert!(log.entries_decoded() <= 4 * Fixture::PER_COMMIT, "{log:?}");
}
