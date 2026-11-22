//! The engine's full index check reads a segment collection in batches,
//! so it decodes each commit's manifest once (FYLO-CR2).

use std::fs;
use std::path::PathBuf;

use fylo_engine::ReadOnlyEngine;
use fylo_storage_native::{
    CollectionKind, CollectionLayout, DocumentWrite, NativeWriteRoot, PlainDocumentCodec,
    SegmentReadLog,
};
use serde_json::json;

struct Fixture(PathBuf);

impl Fixture {
    const COLLECTION: &str = "items";
    const DOCUMENTS: usize = 200;

    fn new() -> Self {
        let root = fylo_test_support::TestFolder::path()
            .join(format!("fylo-engine-decode-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let writer = NativeWriteRoot::open(&root).unwrap();
        writer
            .create_collection_with_layout(
                Self::COLLECTION,
                CollectionKind::Document,
                None,
                Some(CollectionLayout::Segments),
            )
            .unwrap();
        let writes: Vec<DocumentWrite> = (0..Self::DOCUMENTS)
            .map(|number| DocumentWrite::Put {
                identifier: format!("4VRNF5{number:05}"),
                fields: json!({ "number": number, "team": number % 3 })
                    .as_object()
                    .unwrap()
                    .clone(),
            })
            .collect();
        writer
            .commit_document_writes_with(Self::COLLECTION, &writes, &PlainDocumentCodec)
            .unwrap();
        Self(root)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_full_index_check_decodes_each_commit_once() {
    let fixture = Fixture::new();
    let engine = ReadOnlyEngine::open(&fixture.0).unwrap();
    engine.inspect_indexed(Fixture::COLLECTION).unwrap();
    SegmentReadLog::start();
    let checked = engine.inspect_verified(Fixture::COLLECTION).unwrap();
    let log = SegmentReadLog::finish();
    assert_eq!(checked.inspection.document_count, Fixture::DOCUMENTS);
    assert_eq!(log.entries_decoded(), Fixture::DOCUMENTS, "{log:?}");
    assert_eq!(log.frames_decoded(), 1, "{log:?}");
}
