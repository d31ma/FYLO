//! `fylo pack` and `fylo unpack` (ADR 0016 section 11): a directory root,
//! in the file or the segment layout, into a `.fxdb` database and back to
//! a directory root in the file layout, checked by ADR 0015's oracle.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fylo_storage_native::{
    CollectionKind, CollectionLayout, ContainerFile, NativeRoot, NativeStorageErrorCode,
    NativeWriteRoot, PutDocumentOptions, RootConfig, ValidationMode,
};
use serde_json::{Map, Value, json};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-fxdb-pack-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    /// An empty directory root at `name`.
    fn root(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir(&path).unwrap();
        path
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.0.join("db.fxdb"));
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn writer(root: &Path, layout: CollectionLayout) -> NativeWriteRoot {
    NativeWriteRoot::open(root)
        .unwrap()
        .with_config(RootConfig {
            layout,
            ..RootConfig::default()
        })
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 40_000 + number)
}

/// `chars` characters that neither compress nor repeat another document's,
/// so documents fill `.fxdb` extents by their size.
fn noise(seed: usize, chars: usize) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut state =
        0x9E37_79B9_7F4A_7C15_u64 ^ (seed as u64 + 1).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    (0..chars)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(ALPHABET[(state >> 58) as usize])
        })
        .collect()
}

fn put(writer: &NativeWriteRoot, collection: &str, number: usize, padding: usize) {
    let body = json!({
        "group": number % 3,
        "name": format!("{collection}{number}"),
        "padding": noise(number, padding),
    });
    writer
        .put_document(
            collection,
            &identifier(number),
            body.to_string().as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn version(ordinal: u32) -> String {
    format!("{{\n  \"ordinal\": {ordinal},\n  \"label\": \"v{ordinal}\"\n}}\n")
}

/// One file of a file layout collection: its bytes, modification time,
/// FYLO attributes and mode.
type FileImage = (Vec<u8>, u128, Vec<(String, Vec<u8>)>, u32);

/// Every document file of the file layout collection in `directory`, by
/// path relative to it, with the prefix index's keys.
fn collection_tree(directory: &Path) -> BTreeMap<String, FileImage> {
    let mut tree = BTreeMap::new();
    for namespace in ["docs", ".deleted"] {
        let base = directory.join(namespace);
        for shard in fs::read_dir(&base).unwrap() {
            for entry in fs::read_dir(shard.unwrap().path()).unwrap() {
                let path = entry.unwrap().path();
                let metadata = fs::metadata(&path).unwrap();
                let modified = metadata
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis();
                let relative = path
                    .strip_prefix(directory)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                tree.insert(
                    relative,
                    (
                        fs::read(&path).unwrap(),
                        modified,
                        attributes(&path),
                        mode(&metadata),
                    ),
                );
            }
        }
    }
    for name in ["keys.snapshot", "keys.wal"] {
        let bytes = fs::read(directory.join("index").join(name)).unwrap();
        tree.insert(format!("index/{name}"), (bytes, 0, Vec::new(), 0));
    }
    tree
}

#[cfg(unix)]
fn attributes(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut values: Vec<(String, Vec<u8>)> = xattr::list(path)
        .unwrap()
        .filter_map(|name| {
            let name = name.to_string_lossy().into_owned();
            name.contains("fylo").then(|| {
                let value = xattr::get(path, &name).unwrap().unwrap_or_default();
                (name, value)
            })
        })
        .collect();
    values.sort();
    values
}

#[cfg(not(unix))]
fn attributes(_path: &Path) -> Vec<(String, Vec<u8>)> {
    Vec::new()
}

#[cfg(unix)]
fn mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode(_metadata: &fs::Metadata) -> u32 {
    0
}

fn descriptor(root: &Path, collection: &str) -> Value {
    serde_json::from_slice(
        &fs::read(root.join(format!(".fylo-catalog/collections/{collection}.json"))).unwrap(),
    )
    .unwrap()
}

fn documents(root: &NativeRoot, collection: &str) -> BTreeMap<String, Value> {
    let opened = root.collection(collection).unwrap();
    opened
        .document_ids()
        .unwrap()
        .into_iter()
        .map(|id| {
            let bytes = opened.read_document(&id).unwrap().bytes;
            (id, serde_json::from_slice(&bytes).unwrap())
        })
        .collect()
}

#[test]
fn a_file_layout_root_packs_and_unpacks_to_the_same_files() {
    let scratch = Scratch::new("files");
    let source = scratch.root("source");
    let files = writer(&source, CollectionLayout::Files);
    for collection in ["items", "notes"] {
        files
            .create_collection(collection, CollectionKind::Document, None)
            .unwrap();
    }
    for number in 0..30 {
        put(&files, "items", number, 0);
    }
    for number in 0..3 {
        put(&files, "notes", number, 0);
    }
    let mut metadata = Map::new();
    metadata.insert("team".into(), json!("blue"));
    files
        .set_record_metadata("items", &identifier(5), &metadata, None)
        .unwrap();
    files
        .delete_document("items", &identifier(9), None)
        .unwrap();
    files
        .set_validation_mode("notes", ValidationMode::Warn)
        .unwrap();
    NativeWriteRoot::close_logs_under(files.path()).unwrap();
    // A rebuild leaves the prefix index as one snapshot, which is how an
    // unpack writes it; the documents are untouched.
    for collection in ["items", "notes"] {
        files.rebuild_collection(collection).unwrap();
    }
    let originals: Vec<_> = ["items", "notes"]
        .iter()
        .map(|name| collection_tree(&source.join(".collections").join(name)))
        .collect();
    let bodies = documents(&NativeRoot::open(&source).unwrap(), "items");

    let out = scratch.0.join("db.fxdb");
    let report = files.pack_database(&out, false).unwrap();
    let counts: Vec<_> = report
        .collections
        .iter()
        .map(|packed| (packed.collection.as_str(), packed.documents, packed.deleted))
        .collect();
    assert_eq!(counts, [("items", 29, 1), ("notes", 3, 0)]);
    // Only the database is left beside the source: no staging file.
    assert_eq!(scratch.names(), ["db.fxdb", "source"]);
    let packed = NativeRoot::open(&out).unwrap();
    assert_eq!(
        packed.database_collection_names().unwrap(),
        ["items", "notes"]
    );
    assert_eq!(documents(&packed, "items"), bodies);
    assert_eq!(
        packed
            .collection("items")
            .unwrap()
            .read_custom_metadata(&identifier(5))
            .unwrap()["team"],
        "blue"
    );
    assert_eq!(
        packed.collection("notes").unwrap().validation(),
        Some(ValidationMode::Warn)
    );

    let unpacked = scratch.0.join("unpacked");
    let report = NativeWriteRoot::open(&out)
        .unwrap()
        .unpack_database(&unpacked)
        .unwrap();
    assert_eq!(report.collections.len(), 2);
    for (name, original) in ["items", "notes"].iter().zip(&originals) {
        assert_eq!(
            &collection_tree(&unpacked.join(".collections").join(name)),
            original,
            "{name}"
        );
        assert_eq!(descriptor(&unpacked, name), {
            let mut expected = descriptor(&source, name);
            expected.as_object_mut().unwrap().remove("layout");
            expected
        });
    }
    let reopened = NativeRoot::open(&unpacked).unwrap();
    assert_eq!(
        reopened.collection("items").unwrap().layout(),
        CollectionLayout::Files
    );
    assert_eq!(documents(&reopened, "items"), bodies);
}

#[test]
fn a_segment_root_unpacks_to_what_export_writes() {
    let scratch = Scratch::new("segments");
    let source = scratch.root("source");
    let segments = writer(&source, CollectionLayout::Segments);
    segments
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    for ordinal in 1..=2 {
        segments
            .commit_schema_version("items", ordinal, version(ordinal).as_bytes(), |_| Ok(()))
            .unwrap();
    }
    // Ten mebibytes of documents, which compress to six bits a character,
    // fill more than one 8 MiB extent.
    for number in 0..350 {
        put(&segments, "items", number, 40 * 1024);
    }
    for number in [3, 7, 11] {
        segments
            .delete_document("items", &identifier(number), None)
            .unwrap();
    }
    segments
        .restore_document("items", &identifier(7), None)
        .unwrap();
    let exported = scratch.0.join("exported");
    segments.export_collection("items", &exported).unwrap();
    let expected = collection_tree(&exported);
    let bodies = documents(&NativeRoot::open(&source).unwrap(), "items");

    let out = scratch.0.join("db.fxdb");
    let report = segments.pack_database(&out, false).unwrap();
    assert_eq!(report.collections[0].documents, 348);
    assert_eq!(report.collections[0].deleted, 2);
    assert_eq!(report.collections[0].schema_versions, 2);
    let packed = NativeRoot::open(&out).unwrap();
    assert_eq!(packed.schema_registry_head("items").unwrap(), Some(2));
    assert_eq!(
        packed.read_schema_version("items", 2).unwrap(),
        version(2).as_bytes()
    );
    assert_eq!(documents(&packed, "items"), bodies);
    // The documents filled one extent and went on in a second, and the
    // full one is sealed with the bytes it holds.
    let report = ContainerFile::open_unchecked(&out)
        .unwrap()
        .report()
        .unwrap();
    let census = report.extents.by_kind["segment"];
    assert!(census.extents >= 2 && census.sealed >= 1, "{census:?}");
    assert!(census.live_bytes > 7 * 1024 * 1024, "{census:?}");
    // The packed database takes writes like any other.
    put(&NativeWriteRoot::open(&out).unwrap(), "items", 500, 0);

    let unpacked = scratch.0.join("unpacked");
    let report = NativeWriteRoot::open(&out)
        .unwrap()
        .unpack_database(&unpacked)
        .unwrap();
    assert_eq!(report.collections[0].documents, 349);
    let mut tree = collection_tree(&unpacked.join(".collections/items"));
    let written = tree
        .keys()
        .find(|path| path.contains(&identifier(500)))
        .cloned()
        .unwrap();
    tree.remove(&written);
    let mut expected_keys = expected.clone();
    let keys_of = |tree: &mut BTreeMap<String, FileImage>| tree.remove("index/keys.snapshot");
    let (unpacked_keys, exported_keys) = (keys_of(&mut tree), keys_of(&mut expected_keys));
    assert_eq!(tree, expected_keys);
    assert!(
        unpacked_keys.unwrap().0.len() > exported_keys.unwrap().0.len(),
        "the document written after the pack has index keys too"
    );
    let reopened = NativeRoot::open(&unpacked).unwrap();
    assert_eq!(reopened.schema_registry_head("items").unwrap(), Some(2));
    for ordinal in 1..=2 {
        assert_eq!(
            reopened.read_schema_version("items", ordinal).unwrap(),
            version(ordinal).as_bytes()
        );
    }
    let state: Value = serde_json::from_slice(
        &fs::read(unpacked.join(".fylo-transactions/.collections/items/state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["format"], "fylo.collection-generation.v3");
    // The unpacked root takes writes too.
    put(&NativeWriteRoot::open(&unpacked).unwrap(), "items", 501, 0);
}

#[test]
fn a_pack_without_writes_after_it_unpacks_to_the_export_byte_for_byte() {
    let scratch = Scratch::new("export");
    let source = scratch.root("source");
    let segments = writer(&source, CollectionLayout::Segments);
    segments
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    for number in 0..40 {
        put(&segments, "items", number, 100);
    }
    segments
        .delete_document("items", &identifier(4), None)
        .unwrap();
    let exported = scratch.0.join("exported");
    segments.export_collection("items", &exported).unwrap();
    let out = scratch.0.join("db.fxdb");
    segments.pack_database(&out, false).unwrap();
    let unpacked = scratch.0.join("unpacked");
    NativeWriteRoot::open(&out)
        .unwrap()
        .unpack_database(&unpacked)
        .unwrap();
    assert_eq!(
        collection_tree(&unpacked.join(".collections/items")),
        collection_tree(&exported)
    );
}

#[test]
fn pack_refuses_what_a_database_does_not_hold() {
    let scratch = Scratch::new("refuse");
    let out = scratch.0.join("db.fxdb");

    let buckets = scratch.root("buckets");
    let files = writer(&buckets, CollectionLayout::Files);
    files
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    files
        .create_collection("assets", CollectionKind::File, None)
        .unwrap();
    let error = files.pack_database(&out, false).unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::LayoutUnsupported);
    assert!(error.to_string().contains("buckets"), "{error}");
    assert!(error.to_string().contains("\"assets\""), "{error}");

    let versioned = scratch.root("versioned");
    fs::create_dir_all(versioned.join(".fylo-vcs")).unwrap();
    let error = writer(&versioned, CollectionLayout::Files)
        .pack_database(&out, false)
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::LayoutUnsupported);
    assert!(error.to_string().contains("version control"), "{error}");

    let queued = scratch.root("queued");
    fs::create_dir_all(queued.join(".fylo-queue/v1")).unwrap();
    let error = writer(&queued, CollectionLayout::Files)
        .pack_database(&out, false)
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::LayoutUnsupported);
    assert!(error.to_string().contains("embedded queue"), "{error}");
    // Nothing was written for any of them.
    assert!(!out.exists());
    assert_eq!(scratch.names(), ["buckets", "queued", "versioned"]);

    // An existing output is never replaced.
    let plain = scratch.root("plain");
    let writer = writer(&plain, CollectionLayout::Files);
    writer
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    put(&writer, "items", 1, 0);
    fs::write(&out, b"keep me").unwrap();
    let error = writer.pack_database(&out, false).unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
    assert_eq!(fs::read(&out).unwrap(), b"keep me");
    fs::remove_file(&out).unwrap();

    // Pack takes a directory root, and unpack a database.
    writer.pack_database(&out, false).unwrap();
    let database = NativeWriteRoot::open(&out).unwrap();
    let error = database
        .pack_database(&scratch.0.join("again.fxdb"), false)
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
    let error = writer
        .unpack_database(&scratch.0.join("unpacked"))
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Unsupported);
    // Unpack refuses a non-empty output and leaves it alone.
    let occupied = scratch.root("occupied");
    fs::write(occupied.join("note.txt"), b"mine").unwrap();
    assert!(database.unpack_database(&occupied).is_err());
    assert_eq!(fs::read(occupied.join("note.txt")).unwrap(), b"mine");
}

#[test]
fn a_stopped_packs_staging_file_is_removed_by_the_next_pack() {
    let scratch = Scratch::new("staging");
    let source = scratch.root("source");
    let files = writer(&source, CollectionLayout::Files);
    files
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    put(&files, "items", 1, 0);
    // What a pack that stopped after building its staging file leaves.
    fs::write(scratch.0.join(".db.fxdb.pack-staging"), b"half a database").unwrap();
    fs::write(scratch.0.join(".db.fxdb.pack-staging.lease.3"), b"{}").unwrap();
    let out = scratch.0.join("db.fxdb");
    files.pack_database(&out, false).unwrap();
    assert_eq!(scratch.names(), ["db.fxdb", "source"]);
    assert_eq!(
        documents(&NativeRoot::open(&out).unwrap(), "items").len(),
        1
    );
}
