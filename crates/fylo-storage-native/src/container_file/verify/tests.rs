use crate::container_file::{ContainerFile, ContainerProblemSeverity, ContainerVerifyReport};
use fylo_format::{ContainerCheckpointSlot, ContainerExtentKind, ContainerSuperblock};
use std::path::{Path, PathBuf};

// 1 MiB extents keep the test files small.
const SHIFT: u8 = ContainerSuperblock::MIN_EXTENT_SHIFT;
const EXTENT_BYTES: u64 = 1 << SHIFT;
const PAYLOAD: u64 = 4096;
const SEGMENT: ContainerExtentKind = ContainerExtentKind::Segment;
const SECRET: &[u8] = b"correct horse battery staple";

struct Database(PathBuf);

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn database(name: &str, secret: Option<&[u8]>) -> (Database, PathBuf) {
    let dir = std::env::temp_dir().join(crate::write::unique_name(name));
    std::fs::create_dir_all(&dir).expect("create directory");
    let path = dir.join("test.fxdb");
    match secret {
        Some(secret) => ContainerFile::create_encrypted(&path, SHIFT, secret),
        None => ContainerFile::create(&path, SHIFT),
    }
    .expect("create container");
    (Database(dir), path)
}

/// A collection with one segment and both snapshots, checkpointed: extent
/// 1 is the directory, 3 the segment, 4 the locations snapshot, 5 the index
/// snapshot and 6 the map; extent 2, the first map, is free.
fn populated(name: &str, secret: Option<&[u8]>) -> (Database, PathBuf) {
    let (guard, path) = database(name, secret);
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    if let Some(secret) = secret {
        container.unlock(secret).expect("unlock");
    }
    let users = container.add_collection("users", b"").expect("add");
    let (block, id) = (users.block, users.header.id);
    let segment = container.allocate(SEGMENT, id).expect("allocate segment");
    let locations = container
        .write_snapshot(id, ContainerExtentKind::Locations, 1, &[7; 5000])
        .expect("write locations");
    let index = container
        .write_snapshot(id, ContainerExtentKind::Index, 1, b"users/1\n")
        .expect("write index");
    container
        .record_checkpoint(
            block,
            ContainerCheckpointSlot {
                checkpoint: 1,
                lsn: 1,
                extent: segment.index,
                extent_sequence: segment.sequence,
                offset: 0,
                locations: Some(locations),
                index: Some(index),
                dictionary: None,
            },
        )
        .expect("record checkpoint");
    container.checkpoint().expect("checkpoint");
    assert_eq!(
        (segment.index, locations.first_extent, index.first_extent),
        (3, 4, 5)
    );
    (guard, path)
}

fn verified(path: &Path) -> ContainerVerifyReport {
    ContainerFile::open_reader(path)
        .expect("open to read")
        .verify()
        .expect("verify")
}

fn codes(report: &ContainerVerifyReport) -> Vec<(ContainerProblemSeverity, &'static str)> {
    report
        .problems
        .iter()
        .map(|problem| (problem.severity, problem.code))
        .collect()
}

fn write_at(path: &Path, offset: u64, bytes: &[u8]) {
    let mut file = std::fs::read(path).expect("read");
    let at = usize::try_from(offset).expect("offset");
    file[at..at + bytes.len()].copy_from_slice(bytes);
    std::fs::write(path, file).expect("damage");
}

fn flip(path: &Path, offset: u64) {
    let bytes = std::fs::read(path).expect("read");
    let at = usize::try_from(offset).expect("offset");
    write_at(path, offset, &[bytes[at] ^ 0x40]);
}

#[test]
fn a_clean_plain_file_verifies() {
    let (_guard, path) = populated("fxdb-verify-plain", None);
    let report = verified(&path);
    assert!(report.ok, "{report:?}");
    assert!(report.problems.is_empty(), "{report:?}");
    assert_eq!(report.checked_extents, 5);
    // The first map's extent is free with its old header in place.
    assert_eq!(report.unmapped_headers, 1);
    let writer = ContainerFile::open_unchecked(&path).expect("open container");
    assert_eq!(writer.verify().expect("verify"), report);
    let json = serde_json::to_value(&report).expect("serialize");
    assert_eq!(json["ok"], serde_json::Value::Bool(true));
    assert_eq!(json["checkedExtents"], 5);
    assert_eq!(json["unmappedHeaders"], 1);
}

#[test]
fn a_clean_encrypted_file_verifies_without_its_key() {
    let (_guard, path) = populated("fxdb-verify-encrypted", Some(SECRET));
    let reader = ContainerFile::open_reader(&path).expect("open to read");
    assert!(reader.is_locked());
    let report = reader.verify().expect("verify");
    assert!(report.ok && report.problems.is_empty(), "{report:?}");
    assert_eq!(report.checked_extents, 5);
    let mut unlocked = ContainerFile::open_reader(&path).expect("open to read");
    unlocked.unlock(SECRET).expect("unlock");
    assert_eq!(unlocked.verify().expect("verify"), report);
}

#[test]
fn a_damaged_sealed_directory_block_is_found_without_the_key() {
    let (_guard, path) = populated("fxdb-verify-sealed-block", Some(SECRET));
    flip(&path, EXTENT_BYTES + PAYLOAD + 100);
    let report = verified(&path);
    assert!(!report.ok);
    assert_eq!(
        codes(&report),
        vec![(ContainerProblemSeverity::Error, "directoryBlockDamaged")]
    );
    assert_eq!(report.problems[0].extent, Some(1));
}

#[test]
fn a_damaged_header_of_an_extent_in_use_is_an_error() {
    let (_guard, path) = populated("fxdb-verify-header", None);
    flip(&path, 3 * EXTENT_BYTES + 20);
    let report = verified(&path);
    assert!(!report.ok);
    assert_eq!(
        codes(&report),
        vec![(ContainerProblemSeverity::Error, "extentHeaderMismatch")]
    );
    assert_eq!(report.problems[0].extent, Some(3));
}

#[test]
fn an_extent_of_a_collection_that_does_not_exist_is_an_error() {
    let (_guard, path) = populated("fxdb-verify-owner", None);
    {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        container.allocate(SEGMENT, 9).expect("allocate");
        container.checkpoint().expect("checkpoint");
    }
    let report = verified(&path);
    assert_eq!(
        codes(&report),
        vec![(ContainerProblemSeverity::Error, "unknownOwner")]
    );
    assert_eq!(report.problems[0].extent, Some(2));
}

#[test]
fn a_flipped_snapshot_byte_fails_its_hash() {
    let (_guard, path) = populated("fxdb-verify-snapshot", None);
    flip(&path, 4 * EXTENT_BYTES + PAYLOAD + 3000);
    let report = verified(&path);
    assert!(!report.ok);
    assert_eq!(
        codes(&report),
        vec![(ContainerProblemSeverity::Error, "snapshotHashMismatch")]
    );
    assert_eq!(report.problems[0].extent, Some(4));
}

#[test]
fn a_damaged_superblock_copy_out_of_force_is_only_a_warning() {
    let (_guard, path) = populated("fxdb-verify-superblock", None);
    // The checkpoint wrote copy B, so copy A is the older one.
    flip(&path, 200);
    let report = verified(&path);
    assert!(report.ok, "{report:?}");
    assert_eq!(
        codes(&report),
        vec![(ContainerProblemSeverity::Warning, "superblockCopyDamaged")]
    );
}

#[test]
fn a_torn_checkpoint_slot_is_only_a_warning() {
    let (_guard, path) = populated("fxdb-verify-torn-slot", None);
    // Slot A holds the checkpoint; half of slot B was written.
    let slot_b = EXTENT_BYTES + PAYLOAD + 3456 + 256;
    write_at(&path, slot_b, &[0x5A; 100]);
    let report = verified(&path);
    assert!(report.ok, "{report:?}");
    assert_eq!(
        codes(&report),
        vec![(ContainerProblemSeverity::Warning, "tornSlot")]
    );
}

#[test]
fn a_truncated_file_is_an_error_unless_only_free_extents_are_cut() {
    let (_guard, path) = database("fxdb-verify-truncated", None);
    {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        let id = container
            .add_collection("users", b"")
            .expect("add")
            .header
            .id;
        container.allocate(SEGMENT, id).expect("allocate");
        container.allocate(SEGMENT, id).expect("allocate");
        // The first checkpoint puts the map at 5 and frees 2; the second
        // moves it back to 2 and frees 5.
        container.checkpoint().expect("checkpoint");
        container.checkpoint().expect("checkpoint");
        assert_eq!(container.superblock().map_first_extent, 2);
    }
    assert!(verified(&path).problems.is_empty());
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open to cut");
    file.set_len(5 * EXTENT_BYTES).expect("cut the free tail");
    let free_cut = verified(&path);
    assert!(free_cut.ok, "{free_cut:?}");
    assert_eq!(
        codes(&free_cut),
        vec![(ContainerProblemSeverity::Warning, "fileTooShort")]
    );
    file.set_len(4 * EXTENT_BYTES + 2 * PAYLOAD)
        .expect("cut into a segment");
    let segment_cut = verified(&path);
    assert!(!segment_cut.ok);
    assert_eq!(
        codes(&segment_cut),
        vec![(ContainerProblemSeverity::Error, "fileTooShort")]
    );
    assert_eq!(segment_cut.problems[0].extent, Some(4));
}

#[test]
fn an_extent_taken_after_the_checkpoint_is_counted_not_reported() {
    let (_guard, path) = database("fxdb-verify-unmapped", None);
    {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        let id = container
            .add_collection("users", b"")
            .expect("add")
            .header
            .id;
        container.allocate(SEGMENT, id).expect("allocate");
        container.checkpoint().expect("checkpoint");
        // Reuses extent 2, the first map, then grows the file.
        assert_eq!(container.allocate(SEGMENT, id).expect("allocate").index, 2);
        assert_eq!(container.allocate(SEGMENT, id).expect("allocate").index, 5);
    }
    let report = verified(&path);
    assert!(report.ok && report.problems.is_empty(), "{report:?}");
    assert_eq!(report.checked_extents, 3);
    assert_eq!(report.unmapped_headers, 2);
}
