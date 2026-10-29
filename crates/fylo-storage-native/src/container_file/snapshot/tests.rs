use crate::container_allocator::ContainerAllocatorError;
use crate::container_file::{ContainerError, ContainerFile};
use fylo_format::{
    ContainerCheckpointSlot, ContainerExtentKind, ContainerFormatError, ContainerSnapshotRef,
    ContainerSuperblock,
};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

// 1 MiB extents keep the test files small.
const SHIFT: u8 = ContainerSuperblock::MIN_EXTENT_SHIFT;
const EXTENT_BYTES: u64 = 1 << SHIFT;
const LOCATIONS: ContainerExtentKind = ContainerExtentKind::Locations;
const INDEX: ContainerExtentKind = ContainerExtentKind::Index;
const USERS: u32 = 1;
const CHECKPOINT: u64 = 1;

struct Database(PathBuf);

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn created(name: &str) -> (Database, PathBuf) {
    let dir = std::env::temp_dir().join(crate::write::unique_name(name));
    std::fs::create_dir_all(&dir).expect("create directory");
    let path = dir.join("test.fxdb");
    ContainerFile::create(&path, SHIFT).expect("create container");
    (Database(dir), path)
}

/// Bytes that differ at every position, so a payload written to the wrong
/// offset cannot round-trip.
fn pattern(length: usize) -> Vec<u8> {
    (0..length)
        .map(|at| u8::try_from((at * 31 + at / 251) % 251).expect("byte"))
        .collect()
}

fn checkpoint_with(
    locations: ContainerSnapshotRef,
    index: ContainerSnapshotRef,
) -> ContainerCheckpointSlot {
    ContainerCheckpointSlot {
        checkpoint: 1,
        lsn: 10,
        extent: 3,
        extent_sequence: 3,
        offset: 0,
        locations: Some(locations),
        index: Some(index),
        schemas: None,
        dictionary: None,
    }
}

fn corrupt(error: &ContainerError) -> bool {
    error.code() == "EFXDB_CORRUPT"
}

#[test]
fn snapshots_of_every_size_round_trip() {
    let (_guard, path) = created("fxdb-snapshot-sizes");
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    let capacity = usize::try_from(container.payload_capacity()).expect("capacity");
    let two_and_a_half_mib = 5 * usize::try_from(EXTENT_BYTES).expect("extent") / 2;
    for (length, extents) in [
        (0, 1),
        (100, 1),
        (capacity, 1),
        (capacity + 1, 2),
        (two_and_a_half_mib, 3),
    ] {
        let bytes = pattern(length);
        let snapshot = container
            .write_snapshot(USERS, LOCATIONS, CHECKPOINT, &bytes)
            .expect("write snapshot");
        assert_eq!(snapshot.extent_count, extents, "{length} bytes");
        assert_eq!(snapshot.length, length as u64);
        assert_eq!(snapshot.sha256, <[u8; 32]>::from(Sha256::digest(&bytes)));
        for extent in snapshot.first_extent..snapshot.first_extent + u64::from(extents) {
            let header = container
                .expect_snapshot_extent(extent, Some((LOCATIONS, USERS)))
                .expect("snapshot header");
            assert_eq!(header.index, extent);
        }
        let read = container
            .read_snapshot(USERS, LOCATIONS, CHECKPOINT, &snapshot, length as u64)
            .expect("read snapshot");
        assert_eq!(read, bytes, "{length} bytes");
    }
    // Runs are consecutive and follow one another: 1 + 1 + 1 + 2 + 3 extents
    // after the directory and map.
    assert_eq!(container.allocator().extent_count(), 3 + 8);
    assert_eq!(
        std::fs::metadata(&path).expect("stat").len(),
        11 * EXTENT_BYTES
    );
}

#[test]
fn a_snapshot_survives_a_checkpoint_and_reopening_to_read() {
    let (_guard, path) = created("fxdb-snapshot-reopen");
    let locations = pattern(2 * usize::try_from(EXTENT_BYTES).expect("extent"));
    let index = b"users/1\nusers/2\n".to_vec();
    {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        let users = container.add_collection("users", b"").expect("add");
        let (block, id) = (users.block, users.header.id);
        assert_eq!(id, USERS);
        let locations_ref = container
            .write_snapshot(id, LOCATIONS, CHECKPOINT, &locations)
            .expect("write locations");
        let index_ref = container
            .write_snapshot(id, INDEX, CHECKPOINT, &index)
            .expect("write index");
        container
            .record_checkpoint(block, checkpoint_with(locations_ref, index_ref))
            .expect("record checkpoint");
        container.checkpoint().expect("checkpoint");
    }
    let reader = ContainerFile::open_reader(&path).expect("open to read");
    let slot = reader.collections()[0].checkpoint.expect("checkpoint slot");
    let locations_ref = slot.locations.expect("locations snapshot");
    let index_ref = slot.index.expect("index snapshot");
    assert_eq!(locations_ref.extent_count, 3);
    assert_eq!(
        reader
            .read_snapshot(USERS, LOCATIONS, CHECKPOINT, &locations_ref, u64::MAX)
            .expect("read locations"),
        locations
    );
    assert_eq!(
        reader
            .read_snapshot(USERS, INDEX, CHECKPOINT, &index_ref, 1024)
            .expect("read index"),
        index
    );
    // The checkpointed map shows the runs sealed, with their bytes live.
    let report = reader.report().expect("report");
    let census = &report.extents.by_kind["locations"];
    assert_eq!((census.extents, census.sealed), (3, 3));
    assert_eq!(census.live_bytes, locations.len() as u64);
    assert_eq!(
        report.extents.by_kind["index"].live_bytes,
        index.len() as u64
    );
    let json = serde_json::to_value(&report).expect("serialize");
    assert_eq!(json["extents"]["byKind"]["locations"]["sealed"], 3);
    assert_eq!(json["extents"]["byKind"]["index"]["extents"], 1);
}

#[test]
fn a_tampered_snapshot_is_corrupt() {
    let (_guard, path) = created("fxdb-snapshot-tampered");
    let bytes = pattern(3 * usize::try_from(EXTENT_BYTES).expect("extent") / 2);
    let snapshot = {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        let snapshot = container
            .write_snapshot(USERS, LOCATIONS, CHECKPOINT, &bytes)
            .expect("write snapshot");
        container.checkpoint().expect("checkpoint");
        snapshot
    };
    let mut file = std::fs::read(&path).expect("read container");
    // A byte in the second extent's payload.
    let at = (snapshot.first_extent + 1) * EXTENT_BYTES + 4096 + 77;
    file[usize::try_from(at).expect("offset")] ^= 0x01;
    std::fs::write(&path, &file).expect("tamper");
    let container = ContainerFile::open_reader(&path).expect("open to read");
    let error = container
        .read_snapshot(USERS, LOCATIONS, CHECKPOINT, &snapshot, u64::MAX)
        .expect_err("tampered");
    assert!(matches!(
        error,
        ContainerError::Format(ContainerFormatError::ChecksumMismatch)
    ));
    assert!(corrupt(&error));
}

#[test]
fn a_snapshot_read_as_another_kind_or_collection_is_refused() {
    let (_guard, path) = created("fxdb-snapshot-owner");
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    let snapshot = container
        .write_snapshot(USERS, LOCATIONS, CHECKPOINT, b"locations")
        .expect("write snapshot");
    for (owner, kind) in [
        (USERS, INDEX),
        (USERS, ContainerExtentKind::Schemas),
        (USERS + 1, LOCATIONS),
    ] {
        let error = container
            .read_snapshot(owner, kind, CHECKPOINT, &snapshot, u64::MAX)
            .expect_err("someone else's snapshot");
        assert!(matches!(
            error,
            ContainerError::Format(ContainerFormatError::InvalidField)
        ));
        assert!(corrupt(&error));
    }
    // Only the derived snapshot kinds live in a run.
    for kind in [
        ContainerExtentKind::Segment,
        ContainerExtentKind::Directory,
        ContainerExtentKind::Map,
    ] {
        assert!(matches!(
            container.write_snapshot(USERS, kind, CHECKPOINT, b"x"),
            Err(ContainerError::Format(ContainerFormatError::InvalidField))
        ));
        assert!(matches!(
            container.read_snapshot(USERS, kind, CHECKPOINT, &snapshot, u64::MAX),
            Err(ContainerError::Format(ContainerFormatError::InvalidField))
        ));
    }
    // A reference into the directory, or past the end of the file.
    let directory = ContainerSnapshotRef {
        first_extent: 1,
        ..snapshot
    };
    assert!(matches!(
        container.read_snapshot(USERS, LOCATIONS, CHECKPOINT, &directory, u64::MAX),
        Err(ContainerError::Format(ContainerFormatError::InvalidField))
    ));
    for first_extent in [0, snapshot.first_extent + 1, u64::MAX] {
        let outside = ContainerSnapshotRef {
            first_extent,
            ..snapshot
        };
        assert!(matches!(
            container.read_snapshot(USERS, LOCATIONS, CHECKPOINT, &outside, u64::MAX),
            Err(ContainerError::Allocator(
                ContainerAllocatorError::OutOfRange
            ))
        ));
    }
}

#[test]
fn a_snapshot_over_the_limit_is_refused_before_it_is_read() {
    let (_guard, path) = created("fxdb-snapshot-limit");
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    let snapshot = container
        .write_snapshot(USERS, LOCATIONS, CHECKPOINT, &pattern(1000))
        .expect("write snapshot");
    assert!(matches!(
        container.read_snapshot(USERS, LOCATIONS, CHECKPOINT, &snapshot, 999),
        Err(ContainerError::Format(ContainerFormatError::Oversized))
    ));
    container
        .read_snapshot(USERS, LOCATIONS, CHECKPOINT, &snapshot, 1000)
        .expect("at the limit");
    // Neither length is ever allocated: one is over the limit, the other
    // more than its run can hold.
    let huge = ContainerSnapshotRef {
        length: u64::MAX,
        ..snapshot
    };
    let error = container
        .read_snapshot(USERS, LOCATIONS, CHECKPOINT, &huge, 1 << 20)
        .expect_err("over the limit");
    assert!(matches!(
        error,
        ContainerError::Format(ContainerFormatError::Oversized)
    ));
    assert!(corrupt(&error));
    let overlong = ContainerSnapshotRef {
        length: container.payload_capacity() + 1,
        ..snapshot
    };
    assert!(matches!(
        container.read_snapshot(USERS, LOCATIONS, CHECKPOINT, &huge, u64::MAX),
        Err(ContainerError::Format(ContainerFormatError::InvalidField))
    ));
    assert!(matches!(
        container.read_snapshot(USERS, LOCATIONS, CHECKPOINT, &overlong, u64::MAX),
        Err(ContainerError::Format(ContainerFormatError::InvalidField))
    ));
    let no_extents = ContainerSnapshotRef {
        extent_count: 0,
        length: 0,
        ..snapshot
    };
    assert!(matches!(
        container.read_snapshot(USERS, LOCATIONS, CHECKPOINT, &no_extents, u64::MAX),
        Err(ContainerError::Format(ContainerFormatError::InvalidField))
    ));
}

#[test]
fn a_released_snapshot_is_reused_after_a_checkpoint() {
    let (_guard, path) = created("fxdb-snapshot-release");
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    let bytes = pattern(5 * usize::try_from(EXTENT_BYTES).expect("extent") / 2);
    let old = container
        .write_snapshot(USERS, LOCATIONS, CHECKPOINT, &bytes)
        .expect("write snapshot");
    assert_eq!((old.first_extent, old.extent_count), (3, 3));
    container.release_snapshot(&old).expect("release");
    assert_eq!(container.report().expect("report").extents.released, 3);
    // Released twice: refused, and nothing changes.
    assert!(matches!(
        container.release_snapshot(&old),
        Err(ContainerError::Allocator(ContainerAllocatorError::NotInUse))
    ));
    // Not reusable before the checkpoint.
    let held = container
        .write_snapshot(USERS, INDEX, CHECKPOINT, b"index")
        .expect("write index");
    assert_eq!(held.first_extent, 6);
    container.checkpoint().expect("checkpoint");
    // Extent 2 held the old map, 3 to 5 the snapshot; the new map took 7.
    assert_eq!(container.allocator().free_extents(), vec![2, 3, 4, 5]);
    let new = container
        .write_snapshot(USERS, LOCATIONS, CHECKPOINT, &bytes)
        .expect("write again");
    assert_eq!((new.first_extent, new.extent_count), (2, 3));
    assert_eq!(
        container
            .read_snapshot(USERS, LOCATIONS, CHECKPOINT, &new, u64::MAX)
            .expect("read again"),
        bytes
    );
}

#[test]
fn releasing_never_frees_an_extent_outside_a_snapshot() {
    let (_guard, path) = created("fxdb-snapshot-release-guard");
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    let snapshot = container
        .write_snapshot(USERS, LOCATIONS, CHECKPOINT, b"locations")
        .expect("write snapshot");
    let segment = container
        .allocate(ContainerExtentKind::Segment, USERS)
        .expect("allocate segment");
    assert_eq!(segment.index, snapshot.first_extent + 1);
    // A damaged reference that runs into the segment releases nothing.
    let overrun = ContainerSnapshotRef {
        extent_count: 2,
        ..snapshot
    };
    assert!(matches!(
        container.release_snapshot(&overrun),
        Err(ContainerError::Format(ContainerFormatError::InvalidField))
    ));
    assert_eq!(container.report().expect("report").extents.released, 0);
    container
        .read_snapshot(USERS, LOCATIONS, CHECKPOINT, &snapshot, u64::MAX)
        .expect("still readable");
}

#[test]
fn a_reader_cannot_write_or_release_a_snapshot() {
    let (_guard, path) = created("fxdb-snapshot-reader");
    let snapshot = {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        let snapshot = container
            .write_snapshot(USERS, INDEX, CHECKPOINT, b"index")
            .expect("write snapshot");
        container.checkpoint().expect("checkpoint");
        snapshot
    };
    let mut reader = ContainerFile::open_reader(&path).expect("open to read");
    assert!(matches!(
        reader.write_snapshot(USERS, INDEX, CHECKPOINT, b"index"),
        Err(ContainerError::ReadOnly)
    ));
    assert!(matches!(
        reader.release_snapshot(&snapshot),
        Err(ContainerError::ReadOnly)
    ));
    assert_eq!(
        reader
            .read_snapshot(USERS, INDEX, CHECKPOINT, &snapshot, 5)
            .expect("read"),
        b"index"
    );
}

#[test]
fn a_snapshot_the_map_does_not_cover_yet_is_refused_after_reopening() {
    let (_guard, path) = created("fxdb-snapshot-uncheckpointed");
    let snapshot = {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        container
            .write_snapshot(USERS, LOCATIONS, CHECKPOINT, b"locations")
            .expect("write snapshot")
    };
    // Without a map checkpoint the reopened file does not reach the run;
    // the caller rebuilds the snapshot from the segments.
    let reader = ContainerFile::open_reader(&path).expect("open to read");
    let error = reader
        .read_snapshot(USERS, LOCATIONS, CHECKPOINT, &snapshot, u64::MAX)
        .expect_err("outside the checkpointed map");
    assert!(matches!(
        error,
        ContainerError::Allocator(ContainerAllocatorError::OutOfRange)
    ));
    assert!(corrupt(&error));
}

#[test]
fn an_encrypted_snapshot_is_sealed_for_its_place() {
    let dir = std::env::temp_dir().join(crate::write::unique_name("fxdb-snapshot-sealed"));
    std::fs::create_dir_all(&dir).expect("create directory");
    let _guard = Database(dir.clone());
    let path = dir.join("test.fxdb");
    ContainerFile::create_encrypted(&path, SHIFT, b"secret").unwrap();
    let mut container = ContainerFile::open_unchecked(&path).unwrap();
    assert!(matches!(
        container.write_snapshot(USERS, LOCATIONS, CHECKPOINT, b"x"),
        Err(ContainerError::Locked)
    ));
    container.unlock(b"secret").unwrap();
    let bytes = b"user-location-table".repeat(100);
    let snapshot = container
        .write_snapshot(USERS, LOCATIONS, CHECKPOINT, &bytes)
        .unwrap();
    // The stored bytes are sealed: a nonce and tag around the ciphertext.
    assert_eq!(snapshot.length, bytes.len() as u64 + 28);
    let file = std::fs::read(&path).unwrap();
    assert!(
        !file
            .windows(19)
            .any(|window| window == b"user-location-table")
    );
    assert_eq!(
        container
            .read_snapshot(USERS, LOCATIONS, CHECKPOINT, &snapshot, u64::MAX)
            .unwrap(),
        bytes
    );
    for (kind, checkpoint) in [(INDEX, CHECKPOINT), (LOCATIONS, CHECKPOINT + 1)] {
        let refused = container
            .read_snapshot(USERS, kind, checkpoint, &snapshot, u64::MAX)
            .unwrap_err();
        assert_eq!(refused.code(), "EFXDB_CORRUPT");
    }
    let locked = ContainerFile::open_reader(&path).unwrap();
    assert!(matches!(
        locked.read_snapshot(USERS, LOCATIONS, CHECKPOINT, &snapshot, u64::MAX),
        Err(ContainerError::Locked)
    ));
}
