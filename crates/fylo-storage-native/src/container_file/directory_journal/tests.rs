//! A write of a collection block's static part that a power cut tears
//! after its first sector: the directory journal finishes it, a reader
//! sees the new header, verify warns, and a writable open writes it back.

use std::path::{Path, PathBuf};

use fylo_format::ContainerSuperblock;

use super::super::fault::{self, ContainerFault};
use super::super::{ContainerError, ContainerFile};

const SHIFT: u8 = ContainerSuperblock::MIN_EXTENT_SHIFT;
const SECRET: &[u8] = b"correct horse battery staple";

struct Database(PathBuf);

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn database(name: &str, secret: Option<&[u8]>) -> (Database, PathBuf) {
    let directory = std::env::temp_dir().join(crate::write::unique_name(name));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("test.vdbx");
    match secret {
        Some(secret) => ContainerFile::create_encrypted(&path, SHIFT, secret).unwrap(),
        None => ContainerFile::create(&path, SHIFT).unwrap(),
    }
    (Database(directory), path)
}

fn writer(path: &Path, secret: Option<&[u8]>) -> ContainerFile {
    let mut file = ContainerFile::open_unchecked(path).unwrap();
    if let Some(secret) = secret {
        file.unlock(secret).unwrap();
    }
    file
}

fn reader(path: &Path, secret: Option<&[u8]>) -> ContainerFile {
    let mut file = ContainerFile::open_reader(path).unwrap();
    if let Some(secret) = secret {
        file.unlock(secret).unwrap();
    }
    file
}

/// A file with the collections `users` and `orders`.
fn two_collections(name: &str, secret: Option<&[u8]>) -> (Database, PathBuf) {
    let (guard, path) = database(name, secret);
    let mut file = writer(&path, secret);
    file.add_collection("users", b"{}").unwrap();
    file.add_collection("orders", b"{}").unwrap();
    (guard, path)
}

/// Run `change` on a writer with the in-place write torn after its first
/// sector, and check that the change failed there.
fn tear(path: &Path, secret: Option<&[u8]>, change: impl FnOnce(&mut ContainerFile) -> bool) {
    let mut file = writer(path, secret);
    fault::arm(ContainerFault::TornDirectoryBlock);
    assert!(!change(&mut file), "the torn write must fail");
    assert!(!fault::pending(), "the write must have been torn");
}

fn warnings(file: &ContainerFile) -> Vec<&'static str> {
    let report = file.verify().unwrap();
    assert!(report.ok, "{:?}", report.problems);
    report.problems.iter().map(|problem| problem.code).collect()
}

fn names(file: &ContainerFile) -> Vec<(String, bool)> {
    file.collections()
        .iter()
        .map(|collection| (collection.header.name.clone(), collection.header.dropped))
        .collect()
}

/// A reader sees `expected` and a warning; a writer writes the block back,
/// after which verify is quiet.
fn check_restored(path: &Path, secret: Option<&[u8]>, expected: &[(&str, bool)]) {
    let expected: Vec<(String, bool)> = expected
        .iter()
        .map(|(name, dropped)| ((*name).to_owned(), *dropped))
        .collect();
    let read = reader(path, secret);
    assert_eq!(names(&read), expected);
    assert_eq!(warnings(&read), ["directoryBlockTorn"]);
    drop(read);
    assert_eq!(warnings(&reader(path, secret)), ["directoryBlockTorn"]);
    assert_eq!(names(&writer(path, secret)), expected);
    let repaired = reader(path, secret);
    assert_eq!(names(&repaired), expected);
    assert!(warnings(&repaired).is_empty());
}

#[test]
fn a_torn_new_collection_block_is_finished_from_the_journal() {
    let (_guard, path) = database("vdbx-journal-add", None);
    writer(&path, None).add_collection("users", b"{}").unwrap();
    tear(&path, None, |file| {
        file.add_collection("orders", b"{}").is_ok()
    });
    check_restored(&path, None, &[("users", false), ("orders", false)]);
}

#[test]
fn a_torn_drop_is_finished_from_the_journal() {
    let (_guard, path) = two_collections("vdbx-journal-drop", None);
    tear(&path, None, |file| file.drop_collection("orders").is_ok());
    check_restored(&path, None, &[("users", false), ("orders", true)]);
}

#[test]
fn a_torn_descriptor_change_is_finished_from_the_journal() {
    let (_guard, path) = two_collections("vdbx-journal-descriptor", None);
    tear(&path, None, |file| {
        file.replace_descriptor("users", b"{\"kind\":\"document\"}")
            .is_ok()
    });
    check_restored(&path, None, &[("users", false), ("orders", false)]);
    let read = reader(&path, None);
    assert_eq!(
        read.collections()[0].header.descriptor,
        b"{\"kind\":\"document\"}"
    );
}

#[test]
fn a_torn_sealed_block_of_an_encrypted_file_is_finished_from_the_journal() {
    let (_guard, path) = two_collections("vdbx-journal-encrypted", Some(SECRET));
    tear(&path, Some(SECRET), |file| {
        file.drop_collection("users").is_ok()
    });
    check_restored(&path, Some(SECRET), &[("users", true), ("orders", false)]);
}

#[test]
fn a_torn_block_without_its_journal_copy_fails_the_open() {
    let (_guard, path) = two_collections("vdbx-journal-other", None);
    tear(&path, None, |file| file.drop_collection("orders").is_ok());
    let mut bytes = std::fs::read(&path).unwrap();
    let journal = journal_at(&bytes);
    bytes[journal..journal + 2 * 4096].fill(0);
    std::fs::write(&path, bytes).unwrap();
    let refused = ContainerFile::open_reader(&path).err().unwrap();
    assert!(matches!(refused, ContainerError::Format(_)), "{refused:?}");
}

#[test]
fn every_write_alternates_between_the_journal_copies() {
    let (_guard, path) = two_collections("vdbx-journal-copies", None);
    let bytes = std::fs::read(&path).unwrap();
    let journal = journal_at(&bytes);
    let generation = |at: usize| u64::from_le_bytes(bytes[at + 8..at + 16].try_into().unwrap());
    assert_eq!((generation(journal), generation(journal + 4096)), (1, 2));
    let file = reader(&path, None);
    assert_eq!(
        file.journaled_static_part(1)
            .unwrap()
            .map(|part| part.len()),
        Some(3456)
    );
    assert_eq!(file.journaled_static_part(0).unwrap(), None);
}

#[test]
fn a_sealed_copy_carries_no_journal() {
    let (_guard, path) = two_collections("vdbx-journal-sealed", None);
    let mut file = writer(&path, None);
    file.checkpoint().unwrap();
    let out = path.with_file_name("sealed.vdbx");
    file.write_sealed(&out, None).unwrap();
    let sealed = ContainerFile::open_reader(&out).unwrap();
    assert_eq!(names(&sealed).len(), 2);
    assert_eq!(sealed.journaled_static_part(0).unwrap(), None);
    assert_eq!(sealed.journaled_static_part(1).unwrap(), None);
}

/// Where the journal's copy A starts in `bytes`, a plain file.
fn journal_at(bytes: &[u8]) -> usize {
    bytes
        .windows(8)
        .position(|window| window == b"VDXDJRN\0")
        .unwrap()
}
