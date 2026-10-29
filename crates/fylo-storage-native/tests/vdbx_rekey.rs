//! `fylo rekey` (ADR 0016 section 9): an encrypted `.vdbx` database moved
//! to a new secret, a plain one encrypted and an encrypted one decrypted,
//! by a pack of the database into itself, keeping every collection with its
//! documents, deleted documents, schema versions, descriptor, dictionary and
//! prefix index; a wrong old key, a plain database and a sealed one; and a
//! handle opened before the rekey. Every key is generated here.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fylo_storage_native::{
    CollectionKind, ContainerFile, DatabaseKey, DatabaseRekey, NativeRoot, NativeStorageErrorCode,
    NativeWriteRoot, PutDocumentOptions, RootConfig, RootOpenOptions, ValidationMode,
};
use serde_json::{Map, Value, json};

/// Text in every document, which must never appear in an encrypted file.
const CANARY: &str = "WOMBAT-CANARY";

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-vdbx-rekey-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn database(&self) -> PathBuf {
        self.0.join("db.vdbx")
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn contains(&self, text: &str) -> bool {
        fs::read(self.database())
            .unwrap()
            .windows(text.len())
            .any(|window| window == text.as_bytes())
    }

    /// Forget everything this process holds of the database, as a new
    /// process would start.
    fn reopen(&self) {
        NativeRoot::close_database(self.database()).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.database());
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A key generated for one test; no real secret is ever written.
fn generated_key(label: &str) -> DatabaseKey {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    DatabaseKey::new(format!(
        "generated-{label}-{stamp:032x}-{}",
        std::process::id()
    ))
}

fn options(path: &Path, key: Option<&DatabaseKey>) -> RootOpenOptions {
    let options = RootOpenOptions::new(path);
    match key {
        Some(key) => options.with_database_key(key.clone()),
        None => options,
    }
}

fn writer(path: &Path, key: Option<&DatabaseKey>) -> NativeWriteRoot {
    NativeWriteRoot::open_profiled(options(path, key)).unwrap()
}

fn root(path: &Path, key: Option<&DatabaseKey>) -> NativeRoot {
    NativeRoot::open_profiled(options(path, key)).unwrap()
}

fn identifier(number: usize) -> String {
    format!("4VRNF5{:05}", 70_000 + number)
}

/// `chars` characters that neither compress nor repeat another document's.
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

fn body(collection: &str, number: usize, padding: usize) -> Value {
    json!({
        "group": number % 3,
        "name": format!("{collection}{number}"),
        "note": CANARY,
        "padding": noise(number, padding),
    })
}

fn put(writer: &NativeWriteRoot, collection: &str, number: usize, padding: usize) {
    writer
        .put_document(
            collection,
            &identifier(number),
            body(collection, number, padding).to_string().as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

fn version(ordinal: u32) -> String {
    format!("{{\n  \"ordinal\": {ordinal},\n  \"label\": \"v{ordinal}\"\n}}\n")
}

/// The collections of the database at `path`: `notes`, in `warn`
/// validation, and `wide`, at shard width 3, packed from a directory root,
/// since a database sets neither yet; then `items`, written in the
/// database, with two schema versions, metadata, a deleted and a restored
/// document and enough documents to fill two extents and train a
/// dictionary.
fn seed(path: &Path, key: Option<&DatabaseKey>) {
    let parent = path.with_extension("source");
    let source = parent.join("root");
    fs::create_dir_all(&source).unwrap();
    let files = NativeWriteRoot::open(&source).unwrap();
    files
        .create_collection("notes", CollectionKind::Document, None)
        .unwrap();
    for number in 0..4 {
        put(&files, "notes", number, 0);
    }
    files
        .set_validation_mode("notes", ValidationMode::Warn)
        .unwrap();
    let wide = NativeWriteRoot::open(&source)
        .unwrap()
        .with_config(RootConfig {
            shard_width: Some(3),
            ..RootConfig::default()
        });
    wide.create_collection("wide", CollectionKind::Document, None)
        .unwrap();
    for number in 0..5 {
        put(&wide, "wide", number, 0);
    }
    match key {
        Some(key) => files.pack_encrypted_database(path, false, key),
        None => files.pack_database(path, false),
    }
    .unwrap();
    drop((files, wide));
    NativeWriteRoot::close_logs_under(&source).unwrap();
    fs::remove_dir_all(&parent).unwrap();

    let database = writer(path, key);
    database
        .create_collection("items", CollectionKind::Document, None)
        .unwrap();
    for ordinal in 1..=2 {
        database
            .commit_schema_version("items", ordinal, version(ordinal).as_bytes(), |_| Ok(()))
            .unwrap();
    }
    // Three mebibytes of documents, which compress to six bits a
    // character, fill more than one 2 MiB extent, and reach the mebibyte a
    // dictionary trains on.
    for number in 0..80 {
        put(&database, "items", number, 40 * 1024);
    }
    let mut metadata = Map::new();
    metadata.insert("team".into(), json!("blue"));
    database
        .set_record_metadata("items", &identifier(5), &metadata, None)
        .unwrap();
    for number in [3, 7] {
        database
            .delete_document("items", &identifier(number), None)
            .unwrap();
    }
    database
        .restore_document("items", &identifier(7), None)
        .unwrap();
    // A checkpoint folds the records into snapshots, as a live database's
    // would be; the rekey copies the documents either way.
    database.rebuild_collection("items").unwrap();
}

/// Everything a rekey must keep of one collection.
#[derive(Debug, Eq, PartialEq)]
struct Kept {
    documents: BTreeMap<String, Value>,
    deleted: Vec<String>,
    metadata: Option<BTreeMap<String, Value>>,
    schema_head: Option<u32>,
    versions: Vec<Vec<u8>>,
    validation: Option<ValidationMode>,
    shard_width: u32,
    index_keys: Vec<String>,
    descriptor: Vec<u8>,
}

/// What a rekey must keep of every collection of the database at `path`,
/// in the order the database lists them, read with `key`.
fn kept(path: &Path, key: Option<&DatabaseKey>) -> Vec<(String, Kept)> {
    let root = root(path, key);
    let mut file = ContainerFile::open_reader(path).unwrap();
    if let Some(key) = key {
        file.unlock_with_key(key).unwrap();
    }
    let descriptors: BTreeMap<String, Vec<u8>> = file
        .collections()
        .iter()
        .map(|collection| {
            (
                collection.header.name.clone(),
                collection.header.descriptor.clone(),
            )
        })
        .collect();
    root.database_collection_names()
        .unwrap()
        .into_iter()
        .map(|name| {
            let collection = root.collection(&name).unwrap();
            let documents = collection
                .document_ids()
                .unwrap()
                .into_iter()
                .map(|id| {
                    let bytes = collection.read_document(&id).unwrap().bytes;
                    (id, serde_json::from_slice(&bytes).unwrap())
                })
                .collect();
            let schema_head = root.schema_registry_head(&name).unwrap();
            let versions = (1..=schema_head.unwrap_or(0))
                .map(|ordinal| root.read_schema_version(&name, ordinal).unwrap())
                .collect();
            let mut index_keys: Vec<String> =
                String::from_utf8(collection.index_snapshot().unwrap().as_bytes().to_vec())
                    .unwrap()
                    .lines()
                    .map(ToOwned::to_owned)
                    .collect();
            index_keys.sort();
            let verified = collection.verify_index_references().unwrap();
            assert!(verified.reference_integrity, "{name}: {verified:?}");
            let kept = Kept {
                documents,
                deleted: collection.deleted_document_ids().unwrap(),
                metadata: collection.read_custom_metadata(&identifier(5)).ok(),
                schema_head,
                versions,
                validation: collection.validation(),
                shard_width: collection.shard_width(),
                index_keys,
                descriptor: descriptors[&name].clone(),
            };
            (name, kept)
        })
        .collect()
}

/// Require that `after` keeps everything `before` held, naming the first
/// field that differs without printing the documents.
fn require_kept(before: &[(String, Kept)], after: &[(String, Kept)]) {
    let names = |kept: &[(String, Kept)]| -> Vec<String> {
        kept.iter().map(|(name, _)| name.clone()).collect()
    };
    assert_eq!(names(after), names(before));
    for ((name, before), (_, after)) in before.iter().zip(after) {
        assert!(after.documents == before.documents, "{name}: documents");
        assert_eq!(after.deleted, before.deleted, "{name}: deleted");
        assert_eq!(after.metadata, before.metadata, "{name}: metadata");
        assert_eq!(after.schema_head, before.schema_head, "{name}: schema head");
        assert!(after.versions == before.versions, "{name}: schema versions");
        assert_eq!(after.validation, before.validation, "{name}: validation");
        assert_eq!(after.shard_width, before.shard_width, "{name}: shard width");
        assert_eq!(after.index_keys, before.index_keys, "{name}: index keys");
        // The descriptor is kept but for the stamp of the collection's
        // creation in its file, which names the new file's collection.
        let settings = |kept: &Kept| {
            let mut descriptor: Map<String, Value> =
                serde_json::from_slice(&kept.descriptor).unwrap();
            assert!(descriptor.remove("created").is_some(), "{name}: stamp");
            descriptor
        };
        assert_eq!(settings(after), settings(before), "{name}: descriptor");
    }
}

/// Require that the database at `path` verifies and that its `items`
/// collection has a trained dictionary, as a pack gives it.
fn require_whole(path: &Path, key: Option<&DatabaseKey>) {
    let mut file = ContainerFile::open_reader(path).unwrap();
    if let Some(key) = key {
        file.unlock_with_key(key).unwrap();
    }
    let verified = file.verify().unwrap();
    assert!(verified.ok && verified.problems.is_empty(), "{verified:?}");
    let report = file.report().unwrap();
    let items = report
        .collections
        .iter()
        .find(|collection| collection.name == "items")
        .unwrap();
    assert!(items.dictionary.is_some(), "{report:?}");
    let census = report.extents.by_kind["segment"];
    assert!(census.extents >= 2, "{census:?}");
}

#[test]
fn a_rotation_keeps_every_collection_and_only_the_new_key_opens_it() {
    let scratch = Scratch::new("rotate");
    let path = scratch.database();
    let (old, new) = (generated_key("old"), generated_key("new"));
    seed(&path, Some(&old));
    let before = kept(&path, Some(&old));
    assert_eq!(before.len(), 3);
    let names: Vec<_> = before.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["notes", "wide", "items"]);
    let (_, items) = &before[2];
    assert_eq!(items.documents.len(), 79);
    assert_eq!(items.deleted, [identifier(3)]);
    assert_eq!(items.schema_head, Some(2));
    assert!(items.metadata.is_some());
    assert!(!items.index_keys.is_empty());
    let salt = ContainerFile::open_reader(&path)
        .unwrap()
        .superblock()
        .key_salt;
    scratch.reopen();

    let report = writer(&path, Some(&old))
        .rekey_database(DatabaseRekey::Rotate(&new))
        .unwrap();
    let counts: Vec<_> = report
        .collections
        .iter()
        .map(|packed| {
            (
                packed.collection.as_str(),
                packed.documents,
                packed.deleted,
                packed.schema_versions,
            )
        })
        .collect();
    assert_eq!(
        counts,
        [("notes", 4, 0, 0), ("wide", 5, 0, 0), ("items", 79, 1, 2)]
    );
    assert!(report.sealed.is_none());
    // Only the database is left: no staging file, no lease.
    assert_eq!(scratch.names(), ["db.vdbx"]);

    // The old key is refused at open, as any wrong key is, and without a
    // key the database is locked.
    let refused = NativeRoot::open_profiled(options(&path, Some(&old))).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::KeyRequired);
    assert_eq!(root(&path, None).database_encryption(), Some((true, true)));
    // The new key opens everything as it was, under a new salt.
    let file = ContainerFile::open_reader(&path).unwrap();
    assert!(file.is_encrypted());
    assert_ne!(file.superblock().key_salt, salt);
    require_kept(&before, &kept(&path, Some(&new)));
    require_whole(&path, Some(&new));
    for text in [CANARY, "items1"] {
        assert!(!scratch.contains(text), "{text} is in the encrypted file");
    }
    // It takes writes with the new key, which a fresh open reads.
    put(&writer(&path, Some(&new)), "items", 500, 0);
    scratch.reopen();
    let reopened = root(&path, Some(&new));
    assert_eq!(
        reopened
            .collection("items")
            .unwrap()
            .document_ids()
            .unwrap()
            .len(),
        80
    );
}

#[test]
fn a_wrong_or_missing_old_key_changes_nothing() {
    let scratch = Scratch::new("wrong-key");
    let path = scratch.database();
    let (old, new) = (generated_key("old"), generated_key("new"));
    seed(&path, Some(&old));
    scratch.reopen();
    let bytes = fs::read(&path).unwrap();

    // A wrong old key is refused when the database opens, before any rekey.
    let wrong =
        NativeWriteRoot::open_profiled(options(&path, Some(&generated_key("wrong")))).unwrap_err();
    assert_eq!(wrong.code(), NativeStorageErrorCode::KeyRequired);
    // Without one the database opens locked, and the rekey is refused.
    for rekey in [DatabaseRekey::Rotate(&new), DatabaseRekey::Decrypt] {
        let error = writer(&path, None).rekey_database(rekey).unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::KeyRequired, "{error}");
    }
    scratch.reopen();
    assert!(fs::read(&path).unwrap() == bytes, "the file changed");
    assert_eq!(scratch.names(), ["db.vdbx"]);
    assert_eq!(kept(&path, Some(&old))[2].1.documents.len(), 79);
}

#[test]
fn a_plain_database_is_refused_a_rotation_and_encrypts_and_decrypts_on_request() {
    let scratch = Scratch::new("plain");
    let path = scratch.database();
    let key = generated_key("encrypt");
    seed(&path, None);
    let before = kept(&path, None);
    scratch.reopen();
    let bytes = fs::read(&path).unwrap();

    for rekey in [DatabaseRekey::Rotate(&key), DatabaseRekey::Decrypt] {
        let error = writer(&path, None).rekey_database(rekey).unwrap_err();
        assert_eq!(error.code(), NativeStorageErrorCode::Unsupported, "{error}");
        assert!(error.to_string().contains("plain"), "{error}");
    }
    scratch.reopen();
    assert!(fs::read(&path).unwrap() == bytes, "the file changed");

    // --encrypt: the same collections, sealed with the key.
    writer(&path, None)
        .rekey_database(DatabaseRekey::Encrypt(&key))
        .unwrap();
    assert_eq!(scratch.names(), ["db.vdbx"]);
    assert_eq!(root(&path, None).database_encryption(), Some((true, true)));
    require_kept(&before, &kept(&path, Some(&key)));
    require_whole(&path, Some(&key));
    assert!(!scratch.contains(CANARY));
    let error = writer(&path, Some(&key))
        .rekey_database(DatabaseRekey::Encrypt(&key))
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Unsupported, "{error}");

    // --decrypt: plain again, with every collection as it was.
    scratch.reopen();
    writer(&path, Some(&key))
        .rekey_database(DatabaseRekey::Decrypt)
        .unwrap();
    assert_eq!(
        root(&path, None).database_encryption(),
        Some((false, false))
    );
    require_kept(&before, &kept(&path, None));
    require_whole(&path, None);
    assert!(scratch.contains(CANARY));
}

#[test]
fn a_handle_opened_before_a_rekey_reads_the_old_file_and_never_writes() {
    let scratch = Scratch::new("stale-handle");
    let path = scratch.database();
    let (old, new) = (generated_key("old"), generated_key("new"));
    seed(&path, Some(&old));
    let stale = writer(&path, Some(&old));
    put(&stale, "notes", 10, 0);
    let reader = root(&path, Some(&old));

    writer(&path, Some(&old))
        .rekey_database(DatabaseRekey::Rotate(&new))
        .unwrap();
    // The handles opened before read the old file on, whole.
    assert_eq!(
        reader
            .collection("items")
            .unwrap()
            .document_ids()
            .unwrap()
            .len(),
        79
    );
    let error = stale
        .put_document(
            "notes",
            &identifier(11),
            br#"{"name":"lost"}"#,
            PutDocumentOptions::default(),
        )
        .unwrap_err();
    assert_eq!(
        error.code(),
        NativeStorageErrorCode::RootLeaseLost,
        "{error}"
    );
    assert!(error.to_string().contains("rekey"), "{error}");
    drop((stale, reader));
    // A handle opened now reads the new file, with what was written before.
    let notes = root(&path, Some(&new)).collection("notes").unwrap();
    assert!(notes.read_document(&identifier(10)).is_ok());
    assert!(notes.read_document(&identifier(11)).is_err());
}

#[test]
fn a_stopped_rekeys_staging_file_is_removed_by_the_next_rekey() {
    let scratch = Scratch::new("staging");
    let path = scratch.database();
    let (old, new) = (generated_key("old"), generated_key("new"));
    seed(&path, Some(&old));
    // What a rekey that stopped after building its staging file leaves.
    fs::write(scratch.0.join(".db.vdbx.pack-staging"), b"half a database").unwrap();
    fs::write(scratch.0.join(".db.vdbx.pack-staging.lease.3"), b"{}").unwrap();
    let before = kept(&path, Some(&old));
    scratch.reopen();
    writer(&path, Some(&old))
        .rekey_database(DatabaseRekey::Rotate(&new))
        .unwrap();
    assert_eq!(scratch.names(), ["db.vdbx"]);
    require_kept(&before, &kept(&path, Some(&new)));
}

#[test]
fn a_sealed_database_is_resealed_under_the_new_key() {
    let scratch = Scratch::new("sealed");
    let live = scratch.0.join("live.vdbx");
    let (old, new) = (generated_key("old"), generated_key("new"));
    seed(&live, Some(&old));
    let path = scratch.database();
    writer(&live, Some(&old))
        .pack_sealed_database(&path, false, Some(&old))
        .unwrap();
    let _ = NativeRoot::close_database(&live);
    fs::remove_file(&live).unwrap();
    let before = kept(&path, Some(&old));
    scratch.reopen();

    let report = writer(&path, Some(&old))
        .rekey_database(DatabaseRekey::Rotate(&new))
        .unwrap();
    assert!(report.sealed.is_some_and(|blocks| blocks.blocks > 0));
    assert_eq!(scratch.names(), ["db.vdbx"]);
    let file = ContainerFile::open_reader(&path).unwrap();
    assert!(file.is_sealed() && file.is_encrypted());
    let refused = NativeRoot::open_profiled(options(&path, Some(&old))).unwrap_err();
    assert_eq!(refused.code(), NativeStorageErrorCode::KeyRequired);
    require_kept(&before, &kept(&path, Some(&new)));
    let mut file = ContainerFile::open_reader(&path).unwrap();
    file.unlock_with_key(&new).unwrap();
    let verified = file.verify().unwrap();
    assert!(verified.ok && verified.problems.is_empty(), "{verified:?}");
    assert!(!scratch.contains(CANARY));
    // Still sealed: every write is refused.
    let error = writer(&path, Some(&new))
        .put_document(
            "notes",
            &identifier(20),
            br#"{"name":"new"}"#,
            PutDocumentOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.code(), NativeStorageErrorCode::Unsupported, "{error}");
}
