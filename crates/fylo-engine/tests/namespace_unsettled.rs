use std::fs;
use std::path::PathBuf;

use fylo_engine::{EngineErrorCode, ReadOnlyEngine};
use fylo_storage_native::{
    CollectionKind, CollectionWrites, DocumentWrite, NativeRoot, NativeWriteRoot,
    PlainDocumentCodec,
};
use serde_json::json;

struct TestRoot(PathBuf);

impl TestRoot {
    fn create() -> Self {
        let path = fylo_test_support::TestFolder::path().join(format!(
            "fylo-engine-namespace-unsettled-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        let writer = NativeWriteRoot::open(&path).unwrap();
        for name in ["alpha", "beta"] {
            writer
                .create_collection(name, CollectionKind::Document, None)
                .unwrap();
        }
        Self(fs::canonicalize(path).unwrap())
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const ALPHA_ID: &str = "4VRNF540001";
const BETA_ID: &str = "4VRNF540002";

fn put(identifier: &str) -> DocumentWrite {
    DocumentWrite::Put {
        identifier: identifier.to_owned(),
        fields: json!({ "label": "committed" }).as_object().unwrap().clone(),
    }
}

/// Commit over two collections, then make `beta` look as a power cut left
/// it: its part only in the namespace log, its state back at the start.
fn lose_beta_part(root: &TestRoot) {
    let writer = NativeWriteRoot::open(&root.0).unwrap();
    writer
        .commit_collection_writes_with(
            &[
                CollectionWrites {
                    collection: "alpha".into(),
                    writes: vec![put(ALPHA_ID)],
                },
                CollectionWrites {
                    collection: "beta".into(),
                    writes: vec![put(BETA_ID)],
                },
            ],
            &PlainDocumentCodec,
        )
        .unwrap();
    let document = NativeRoot::open(&root.0)
        .unwrap()
        .collection("beta")
        .unwrap()
        .read_document(BETA_ID)
        .unwrap()
        .path;
    fs::remove_file(document).unwrap();
    let transactions = root.0.join(".fylo-transactions/.collections/beta");
    fs::remove_dir_all(transactions.join("log")).unwrap();
    fs::write(
        transactions.join("state.json"),
        br#"{"format":"fylo.collection-generation.v1","generation":0,"state":"stable"}"#,
    )
    .unwrap();
}

#[test]
fn a_reader_answers_unsettled_until_a_writer_opens_the_root() {
    let root = TestRoot::create();
    lose_beta_part(&root);
    let engine = ReadOnlyEngine::open(&root.0).unwrap();
    let error = engine.get("beta", BETA_ID).unwrap_err();
    assert_eq!(error.code(), EngineErrorCode::NamespaceUnsettled);
    assert!(error.to_string().contains("open the root with a writer"));
    assert!(engine.get("alpha", ALPHA_ID).is_ok());
    NativeWriteRoot::open(&root.0)
        .unwrap()
        .recover_collection("beta")
        .unwrap();
    let document = ReadOnlyEngine::open(&root.0)
        .unwrap()
        .get("beta", BETA_ID)
        .unwrap();
    assert_eq!(document.metadata.id, BETA_ID);
}
