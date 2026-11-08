//! The schema registry's storage half: version files, the descriptor head and
//! the generation-state fence (ADR 0017 section 3).

use std::fs;
use std::path::PathBuf;

use fylo_storage_native::{
    CollectionKind, GenerationState, NativeRoot, NativeStorageErrorCode, NativeWriteRoot,
    PutDocumentOptions,
};
use serde_json::{Value, json};

struct Fixture(PathBuf);

impl Fixture {
    fn create(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-schema-registry-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        NativeWriteRoot::open(&path)
            .unwrap()
            .create_collection("items", CollectionKind::Document, None)
            .unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn writer(&self) -> NativeWriteRoot {
        NativeWriteRoot::open(&self.0).unwrap()
    }

    fn head(&self) -> Option<u32> {
        NativeRoot::open(&self.0)
            .unwrap()
            .schema_registry_head("items")
            .unwrap()
    }

    fn state_path(&self) -> PathBuf {
        self.0
            .join(".fylo-transactions/.collections/items/state.json")
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.state_path()).unwrap()).unwrap()
    }

    fn descriptor(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.0.join(".fylo-catalog/collections/items.json")).unwrap(),
        )
        .unwrap()
    }

    fn commit(&self, ordinal: u32) -> Result<(), fylo_storage_native::NativeStorageError> {
        self.writer()
            .commit_schema_version("items", ordinal, version(ordinal).as_bytes(), |_| Ok(()))
    }

    fn put(&self, identifier: &str) {
        self.writer()
            .put_document(
                "items",
                identifier,
                br#"{"name":"Ada"}"#,
                PutDocumentOptions::default(),
            )
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn version(ordinal: u32) -> String {
    format!(r#"{{"ordinal":{ordinal}}}"#)
}

/// The generation-state check of a release that predates the registry, which
/// knew only the v1 and v2 formats.
fn accepted_before_the_registry(state: &Value) -> bool {
    match state.get("log") {
        None => state["format"] == GenerationState::FORMAT_V1,
        Some(_) => state["format"] == GenerationState::FORMAT_V2,
    }
}

#[test]
fn a_commit_writes_the_version_then_moves_the_descriptor_head() {
    let fixture = Fixture::create("commit");
    assert_eq!(fixture.head(), None);
    fixture.commit(1).unwrap();
    fixture.commit(2).unwrap();
    assert_eq!(fixture.head(), Some(2));
    let descriptor = fixture.descriptor();
    assert_eq!(descriptor["schemas"], "registry", "{descriptor}");
    assert_eq!(descriptor["schemaHead"], 2, "{descriptor}");
    let root = NativeRoot::open(&fixture.0).unwrap();
    assert_eq!(
        root.read_schema_version("items", 1).unwrap(),
        version(1).as_bytes()
    );
    for ordinal in [0, 3] {
        let error = root.read_schema_version("items", ordinal).unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::NotFound);
    }
}

#[test]
fn a_version_that_does_not_follow_the_head_is_refused() {
    let fixture = Fixture::create("ordinal");
    let error = fixture.commit(2).unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::ConcurrentWrite);
    fixture.commit(1).unwrap();
    let error = fixture.commit(1).unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::ConcurrentWrite);
    assert_eq!(fixture.head(), Some(1));
}

#[test]
fn a_refused_verification_commits_nothing() {
    let fixture = Fixture::create("verify");
    let error = fixture
        .writer()
        .commit_schema_version("items", 1, b"{}", |_| {
            Err(fylo_storage_native::NativeStorageError::codec_rejected(
                "dry run failed",
            ))
        })
        .unwrap_err();
    assert!(error.to_string().contains("dry run failed"), "{error}");
    assert_eq!(fixture.head(), None);
    assert!(!fixture.0.join(".fylo-schemas/items/1.json").exists());
    assert!(fixture.descriptor().get("schemaHead").is_none());
}

#[test]
fn the_registry_fences_the_generation_state_from_older_releases() {
    let fixture = Fixture::create("fence");
    fixture.put("4VRNF52JPD0");
    assert!(accepted_before_the_registry(&fixture.state()));

    fixture.commit(1).unwrap();
    let state = fixture.state();
    assert_eq!(state["format"], GenerationState::FORMAT_V3, "{state}");
    assert!(!accepted_before_the_registry(&state), "{state}");

    // Writes, and the checkpoint that empties the log, keep the fence.
    fixture.put("4VRNF52JPD1");
    let state = fixture.state();
    assert_eq!(state["format"], GenerationState::FORMAT_V3, "{state}");
    NativeWriteRoot::close_logs().unwrap();
    let state = fixture.state();
    assert_eq!(state["format"], GenerationState::FORMAT_V3, "{state}");
    assert!(state.get("log").is_none(), "{state}");
    // This binary still reads the fenced collection.
    let collection = NativeRoot::open(&fixture.0)
        .unwrap()
        .collection("items")
        .unwrap();
    assert!(collection.read_document("4VRNF52JPD1").is_ok());
}

#[test]
fn a_generation_format_this_release_does_not_know_is_corrupt_metadata() {
    // The same refusal a release that predates v3 gives a fenced collection.
    let fixture = Fixture::create("future");
    // The commit writes the state without opening a write-ahead log, which a
    // put would leave for another test's `close_logs` to trip over.
    fixture.commit(1).unwrap();
    let mut state = fixture.state();
    state["format"] = json!("fylo.collection-generation.v4");
    fs::write(fixture.state_path(), serde_json::to_vec(&state).unwrap()).unwrap();
    let error = NativeRoot::open(&fixture.0)
        .unwrap()
        .collection("items")
        .unwrap()
        .generation()
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::CorruptMetadata);
    assert_eq!(error.code().as_str(), "ENATIVE_METADATA");
}

#[test]
fn dropping_a_collection_drops_its_registry() {
    let fixture = Fixture::create("drop");
    fixture.commit(1).unwrap();
    let writer = fixture.writer();
    writer.drop_collection("items").unwrap();
    assert!(!fixture.0.join(".fylo-schemas/items").exists());
    writer
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    assert_eq!(fixture.head(), None);
}

#[test]
fn the_import_record_round_trips() {
    let fixture = Fixture::create("import");
    let root = NativeRoot::open(&fixture.0).unwrap();
    assert_eq!(root.schema_import_record().unwrap(), None);
    fixture
        .writer()
        .write_schema_import_record(br#"{"format":"fylo.schema-import.v1"}"#)
        .unwrap();
    assert_eq!(
        root.schema_import_record().unwrap().as_deref(),
        Some(&br#"{"format":"fylo.schema-import.v1"}"#[..])
    );
}
