//! A root on a volume without extended attributes of its own — exFAT, FAT,
//! many SMB and NFS shares — where macOS writes an `AppleDouble` companion,
//! `._<name>`, beside every file and directory that carries one, and Finder a
//! `.DS_Store` in every directory it shows. FYLO walks its own directories
//! there, so every operation has to read the companions as the platform's
//! rather than as damage, and take a companion with its owner when it removes
//! one.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use fylo_storage_native::{
    CollectionKind, CollectionLayout, FilesystemProfile, NativeQueue, NativeRoot, NativeWriteRoot,
    PutDocumentOptions, PutRawFileOptions, QueueClaimOptions, QueuePublishOptions, RootConfig,
    RootLease, RootOpenOptions,
};
use serde_json::{Value, json};

const ITEMS: &str = "items";
const CHILD: &str = "FYLO_COMPANION_CHILD";

/// The start of a real `AppleDouble` file: its magic, version and filler.
const APPLE_DOUBLE: &[u8] =
    b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        \x00\x02\x00\x00\x00\x09";

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-companion-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn root(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir(&path).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = NativeRoot::close_database(self.0.join("db.vdbx"));
        let _ = NativeRoot::close_database(self.0.join("packed.vdbx"));
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn is_companion(name: &str) -> bool {
    name.starts_with("._") || name == ".DS_Store"
}

/// Write a companion beside every file and directory under `directory`, as
/// macOS does when each carries an attribute, and in every directory an
/// orphan whose owner is gone and Finder's state. Returns how many it wrote.
fn plant(directory: &Path) -> usize {
    let mut planted = 0;
    let entries: Vec<fs::DirEntry> = fs::read_dir(directory)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_companion(&name) {
            continue;
        }
        let companion = directory.join(format!("._{name}"));
        if !companion.exists() {
            fs::write(&companion, APPLE_DOUBLE).unwrap();
            planted += 1;
        }
        let kind = entry.file_type().unwrap();
        if kind.is_dir() && !kind.is_symlink() {
            planted += plant(&entry.path());
        }
    }
    for (name, bytes) in [
        ("._4VRNF5ORPHAN.json", APPLE_DOUBLE),
        (".DS_Store", b"\x00\x00\x00\x01Bud1".as_slice()),
    ] {
        let path = directory.join(name);
        if !path.exists() {
            fs::write(path, bytes).unwrap();
            planted += 1;
        }
    }
    planted
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

fn put(writer: &NativeWriteRoot, number: usize) {
    writer
        .put_document(
            ITEMS,
            &identifier(number),
            json!({ "name": format!("item {number}"), "group": number % 3 })
                .to_string()
                .as_bytes(),
            PutDocumentOptions::default(),
        )
        .unwrap();
}

/// Every live document of `items` in the root at `root`, by identifier.
fn bodies(root: &Path) -> BTreeMap<String, Value> {
    let collection = NativeRoot::open(root).unwrap().collection(ITEMS).unwrap();
    collection
        .document_ids()
        .unwrap()
        .into_iter()
        .map(|id| {
            let bytes = collection.read_document(&id).unwrap().bytes;
            (id, serde_json::from_slice(&bytes).unwrap())
        })
        .collect()
}

fn deleted(root: &Path) -> Vec<String> {
    NativeRoot::open(root)
        .unwrap()
        .collection(ITEMS)
        .unwrap()
        .deleted_document_ids()
        .unwrap()
}

/// The prefix index's lines, in order.
fn index_keys(root: &Path) -> Vec<Vec<u8>> {
    let snapshot = NativeRoot::open(root)
        .unwrap()
        .collection(ITEMS)
        .unwrap()
        .index_snapshot()
        .unwrap();
    let mut lines: Vec<Vec<u8>> = snapshot
        .as_bytes()
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::to_vec)
        .collect();
    lines.sort();
    lines
}

fn expected(numbers: &[usize]) -> BTreeMap<String, Value> {
    numbers
        .iter()
        .map(|&number| {
            (
                identifier(number),
                json!({ "name": format!("item {number}"), "group": number % 3 }),
            )
        })
        .collect()
}

/// The names in `directory`, companions included.
fn names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_file_layout_root_works_among_companions() {
    let scratch = Scratch::new("files");
    let root = scratch.root("root");
    let files = writer(&root, CollectionLayout::Files);
    files
        .create_collection(ITEMS, CollectionKind::Document, None)
        .unwrap();
    files
        .commit_schema_version(ITEMS, 1, b"{}\n", |_| Ok(()))
        .unwrap();
    for number in 0..6 {
        put(&files, number);
    }
    files.delete_document(ITEMS, &identifier(5), None).unwrap();
    assert!(plant(&root) > 20);

    // Open and read.
    assert_eq!(bodies(&root), expected(&[0, 1, 2, 3, 4]));
    assert_eq!(deleted(&root), [identifier(5)]);

    // Write, delete and restore.
    let files = writer(&root, CollectionLayout::Files);
    put(&files, 6);
    files.delete_document(ITEMS, &identifier(1), None).unwrap();
    files.restore_document(ITEMS, &identifier(5), None).unwrap();
    plant(&root);
    assert_eq!(bodies(&root), expected(&[0, 2, 3, 4, 5, 6]));
    assert_eq!(deleted(&root), [identifier(1)]);

    // Rebuild the index and recover.
    let before = index_keys(&root);
    files.rebuild_collection(ITEMS).unwrap();
    assert_eq!(index_keys(&root), before);
    plant(&root);
    files.recover_collection(ITEMS).unwrap();
    assert_eq!(bodies(&root), expected(&[0, 2, 3, 4, 5, 6]));

    // A reshard there and back moves every record, and empties every shard
    // of the width it leaves, which then goes despite the companions the
    // records' moves left behind on a host that does not move them.
    let docs = root.join(".collections/items/docs");
    files.reshard_collection(ITEMS, 2).unwrap();
    plant(&root);
    files.reshard_collection(ITEMS, 1).unwrap();
    assert_eq!(bodies(&root), expected(&[0, 2, 3, 4, 5, 6]));
    for name in names(&docs) {
        assert!(
            is_companion(&name) || name.len() == 1,
            "a two-character shard survived: {name}"
        );
        assert!(
            !is_companion(&name) || name.len() != 4,
            "a removed shard's companion survived: {name}"
        );
    }

    // Migrate to segments, compact and export.
    plant(&root);
    let segments = writer(&root, CollectionLayout::Segments);
    segments
        .migrate_layout(ITEMS, CollectionLayout::Segments, true)
        .unwrap();
    plant(&root);
    assert_eq!(bodies(&root), expected(&[0, 2, 3, 4, 5, 6]));
    put(&segments, 7);
    segments
        .delete_document(ITEMS, &identifier(0), None)
        .unwrap();
    segments.compact_collection(ITEMS).unwrap();
    plant(&root);
    assert_eq!(bodies(&root), expected(&[2, 3, 4, 5, 6, 7]));
    segments.recover_collection(ITEMS).unwrap();
    // An output directory holding only companions is still empty.
    let out = scratch.root("export");
    fs::write(out.join("._4VRNF5ORPHAN.json"), APPLE_DOUBLE).unwrap();
    let report = segments.export_collection(ITEMS, &out).unwrap();
    assert_eq!((report.documents, report.deleted), (6, 2));
    let mut exported = Vec::new();
    for namespace in ["docs", ".deleted"] {
        for shard in fs::read_dir(out.join(namespace)).unwrap() {
            exported.extend(names(&shard.unwrap().path()));
        }
    }
    assert_eq!(exported.len(), 8, "{exported:?}");

    // And back to files, then drop: the collection takes its companions.
    segments
        .migrate_layout(ITEMS, CollectionLayout::Files, true)
        .unwrap();
    plant(&root);
    assert_eq!(bodies(&root), expected(&[2, 3, 4, 5, 6, 7]));
    files.drop_collection(ITEMS).unwrap();
    for (directory, owner) in [
        (".collections", "items"),
        (".fylo-catalog/collections", "items.json"),
        (".fylo-schemas", "items"),
    ] {
        let listed = names(&root.join(directory));
        assert!(
            !listed.contains(&owner.to_owned()) && !listed.contains(&format!("._{owner}")),
            "{directory}: {listed:?}"
        );
    }
    files
        .create_collection(ITEMS, CollectionKind::Document, None)
        .unwrap();
    plant(&root);
    assert!(bodies(&root).is_empty());
}

#[test]
fn a_file_collection_and_a_queue_work_among_companions() {
    let scratch = Scratch::new("buckets");
    let root = scratch.root("root");
    let files = writer(&root, CollectionLayout::Files);
    files
        .create_collection("assets", CollectionKind::File, None)
        .unwrap();
    for number in 0..3 {
        files
            .put_raw_file(
                "assets",
                &identifier(number),
                format!("bytes {number}").as_bytes(),
                &PutRawFileOptions {
                    key: format!("/assets/{number}.bin"),
                    extension: ".bin".into(),
                    ..PutRawFileOptions::default()
                },
            )
            .unwrap();
    }
    files
        .delete_document("assets", &identifier(2), None)
        .unwrap();
    plant(&root);
    let assets = NativeRoot::open(&root)
        .unwrap()
        .collection("assets")
        .unwrap();
    assert_eq!(
        assets.raw_file_ids().unwrap(),
        [identifier(0), identifier(1)]
    );
    assert_eq!(assets.deleted_raw_file_ids().unwrap(), [identifier(2)]);
    assert_eq!(
        assets.read_raw_file(&identifier(1)).unwrap().bytes,
        b"bytes 1"
    );
    files.rebuild_collection("assets").unwrap();
    files.drop_collection("assets").unwrap();
    assert!(!names(&root.join(".buckets")).contains(&"._assets".to_owned()));

    let queue = NativeQueue::open(&root).unwrap();
    for number in 0..2 {
        queue
            .publish(
                "jobs",
                json!({ "job": number }),
                &QueuePublishOptions::default(),
            )
            .unwrap();
    }
    plant(&root);
    assert_eq!(queue.stats("jobs", "workers").unwrap().available, 2);
    let claimed = queue
        .claim(
            "jobs",
            "workers",
            QueueClaimOptions {
                max_messages: 10,
                ..QueueClaimOptions::default()
            },
        )
        .unwrap();
    assert_eq!(claimed.len(), 2);
}

#[test]
fn version_control_works_among_companions() {
    let scratch = Scratch::new("versions");
    let root = scratch.root("root");
    let files = writer(&root, CollectionLayout::Files);
    files
        .create_collection(ITEMS, CollectionKind::Document, None)
        .unwrap();
    for number in 0..3 {
        put(&files, number);
    }
    plant(&root);
    let first = files.commit_if_dirty("three").unwrap().unwrap();
    plant(&root);
    put(&files, 3);
    files.delete_document(ITEMS, &identifier(0), None).unwrap();
    files
        .commit_if_dirty("one more, one less")
        .unwrap()
        .unwrap();
    plant(&root);
    assert_eq!(files.commit_if_dirty("nothing").unwrap(), None);

    files.restore_repository_commit(&first, true).unwrap();
    plant(&root);
    assert_eq!(bodies(&root), expected(&[0, 1, 2]));

    let main = files.repository_branches().unwrap()["current"]
        .as_str()
        .unwrap()
        .to_owned();
    // A branch is a ref file under its name, so a name a companion could take
    // is refused.
    for name in ["._feature", "feature/._x", "feature/.DS_Store"] {
        assert!(files.checkout_repository(name, true).is_err(), "{name}");
    }
    let checkout = files.checkout_repository("feature", true).unwrap();
    let branch = PathBuf::from(checkout["root"].as_str().unwrap());
    plant(&root);
    assert_eq!(bodies(&branch), expected(&[0, 1, 2]));
    let feature = NativeWriteRoot::open_with_repository(&branch, &root).unwrap();
    put(&feature, 4);
    feature.commit_if_dirty("four").unwrap().unwrap();
    plant(&root);
    let branches = files.repository_branches().unwrap().to_string();
    assert!(branches.contains("feature"), "{branches}");
    files.checkout_repository(&main, false).unwrap();
    files.merge_repository("feature", None).unwrap();
    plant(&root);
    assert_eq!(bodies(&root), expected(&[0, 1, 2, 4]));
}

/// Run `operation` on the root at `root` in a child process that dies at
/// `failpoint`.
fn run_crash_child(root: &Path, efs: bool, operation: &str, failpoint: &str) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, operation)
        .env("FYLO_COMPANION_ROOT", root)
        .env("FYLO_COMPANION_EFS", if efs { "1" } else { "0" })
        .env("FYLO_RUST_FAILPOINT", failpoint)
        .env("FYLO_RUST_FAILPOINT_ACTION", "abort")
        .status()
        .unwrap();
    assert!(!status.success(), "{operation} did not stop at {failpoint}");
}

fn profiled_writer(root: &Path, efs: bool) -> (NativeWriteRoot, Option<RootLease>) {
    if !efs {
        return (writer(root, CollectionLayout::Files), None);
    }
    let lease = RootLease::acquire_with_profile(root, FilesystemProfile::EfsSidecarV1).unwrap();
    let writer = NativeWriteRoot::open_profiled(
        RootOpenOptions::new(root)
            .with_profile(FilesystemProfile::EfsSidecarV1)
            .with_lease(lease.clone()),
    )
    .unwrap();
    (writer, Some(lease))
}

/// The other half of [`run_crash_child`]: does nothing in an ordinary run.
#[test]
fn crash_child() {
    let Ok(operation) = std::env::var(CHILD) else {
        return;
    };
    let root = PathBuf::from(std::env::var("FYLO_COMPANION_ROOT").unwrap());
    let efs = std::env::var("FYLO_COMPANION_EFS").unwrap() == "1";
    let (writer, _lease) = profiled_writer(&root, efs);
    match operation.as_str() {
        "patch" => writer
            .patch_document(ITEMS, &identifier(1), br#"{"name":"patched"}"#, None)
            .unwrap(),
        "delete" => writer.delete_document(ITEMS, &identifier(2), None).unwrap(),
        "put" => put(&writer, 9),
        other => panic!("unknown child operation {other}"),
    }
    // Not reached: the failpoint aborts the process.
}

/// A write interrupted at `failpoint`, then companions written over what it
/// left, recovers as it would without them.
fn crash_and_recover(efs: bool, operation: &str, failpoint: &str) {
    let scratch = Scratch::new("crash");
    let root = scratch.root("root");
    {
        let (writer, _lease) = profiled_writer(&root, efs);
        writer
            .create_collection(ITEMS, CollectionKind::Document, None)
            .unwrap();
        for number in 0..4 {
            put(&writer, number);
        }
    }
    let before = bodies_profiled(&root, efs);
    plant(&root);
    run_crash_child(&root, efs, operation, failpoint);
    plant(&root);
    {
        let (writer, _lease) = profiled_writer(&root, efs);
        writer.recover_collection(ITEMS).unwrap();
    }
    let after = bodies_profiled(&root, efs);
    let step = format!("{operation} stopped at {failpoint}");
    let changed: Vec<&String> = after
        .keys()
        .chain(before.keys())
        .filter(|id| after.get(*id) != before.get(*id))
        .collect();
    // Recovery rolls the write back or forward, whole.
    assert!(changed.len() <= 2, "{step}: {changed:?}");
    let (writer, _lease) = profiled_writer(&root, efs);
    put(&writer, 8);
    writer.rebuild_collection(ITEMS).unwrap();
}

fn bodies_profiled(root: &Path, efs: bool) -> BTreeMap<String, Value> {
    if !efs {
        return bodies(root);
    }
    let lease = RootLease::acquire_with_profile(root, FilesystemProfile::EfsSidecarV1).unwrap();
    let collection = NativeRoot::open_profiled(
        RootOpenOptions::new(root)
            .with_profile(FilesystemProfile::EfsSidecarV1)
            .with_lease(lease),
    )
    .unwrap()
    .collection(ITEMS)
    .unwrap();
    collection
        .document_ids()
        .unwrap()
        .into_iter()
        .map(|id| {
            let bytes = collection.read_document(&id).unwrap().bytes;
            (id, serde_json::from_slice(&bytes).unwrap())
        })
        .collect()
}

#[test]
fn an_interrupted_logged_write_recovers_among_companions() {
    for failpoint in ["after-log-append", "after-log-sync", "after-log-apply-step"] {
        for operation in ["patch", "delete", "put"] {
            crash_and_recover(false, operation, failpoint);
        }
    }
}

#[cfg(unix)]
#[test]
fn an_interrupted_journaled_write_rolls_back_among_companions() {
    for failpoint in [
        "after-capture",
        "before-commit-marker",
        "after-commit-marker",
    ] {
        for operation in ["patch", "delete", "put"] {
            crash_and_recover(true, operation, failpoint);
        }
    }
}

#[test]
fn a_database_beside_companions_opens_writes_packs_and_unpacks() {
    let scratch = Scratch::new("vdbx");
    let database = scratch.0.join("db.vdbx");
    NativeRoot::create_database(&database).unwrap();
    fs::write(scratch.0.join("._db.vdbx"), APPLE_DOUBLE).unwrap();
    fs::write(scratch.0.join("._db.vdbx.lease.3"), APPLE_DOUBLE).unwrap();
    {
        let writer = NativeWriteRoot::open(&database).unwrap();
        writer
            .create_collection(ITEMS, CollectionKind::Document, None)
            .unwrap();
        for number in 0..4 {
            put(&writer, number);
        }
        writer.delete_document(ITEMS, &identifier(3), None).unwrap();
    }
    NativeRoot::close_database(&database).unwrap();
    assert_eq!(bodies(&database), expected(&[0, 1, 2]));
    put(&NativeWriteRoot::open(&database).unwrap(), 4);
    assert_eq!(bodies(&database), expected(&[0, 1, 2, 4]));

    // Unpack into a directory holding only a companion, which is empty.
    let unpacked = scratch.root("unpacked");
    fs::write(unpacked.join("._4VRNF5ORPHAN.json"), APPLE_DOUBLE).unwrap();
    NativeWriteRoot::open(&database)
        .unwrap()
        .unpack_database(&unpacked)
        .unwrap();
    plant(&unpacked);
    assert_eq!(bodies(&unpacked), expected(&[0, 1, 2, 4]));
    assert_eq!(deleted(&unpacked), [identifier(3)]);

    // And pack the unpacked root, companions and all, back beside them.
    let packed = scratch.0.join("packed.vdbx");
    fs::write(scratch.0.join("._packed.vdbx"), APPLE_DOUBLE).unwrap();
    writer(&unpacked, CollectionLayout::Files)
        .pack_database(&packed, true)
        .unwrap();
    assert_eq!(bodies(&packed), expected(&[0, 1, 2, 4]));
    let beside = names(&scratch.0);
    assert!(
        beside
            .iter()
            .all(|name| !name.starts_with(".packed") && !name.starts_with("._.packed")),
        "a pack left its staging behind: {beside:?}"
    );
}
