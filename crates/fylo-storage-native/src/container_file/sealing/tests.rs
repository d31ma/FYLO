use std::path::{Path, PathBuf};

use fylo_format::{
    ContainerCheckpointSlot, ContainerExtentKind, ContainerGenerationSlot, ContainerSnapshotRef,
    ContainerSuperblock,
};

use crate::{ContainerCipherError, ContainerError, ContainerFile};

// 1 MiB extents keep the test files small.
const SHIFT: u8 = ContainerSuperblock::MIN_EXTENT_SHIFT;
const SEGMENT: ContainerExtentKind = ContainerExtentKind::Segment;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(crate::write::unique_name(name));
    std::fs::create_dir_all(&dir).expect("create directory");
    Scratch(dir)
}

fn payload(number: u64) -> Vec<u8> {
    format!("segment {number} holds these bytes; ")
        .repeat(64)
        .into_bytes()
}

/// A container whose collection owns three segment extents, the middle
/// one of four taken released, so a free extent lies between them, with
/// slots naming the last segment and a snapshot run. Returns the source
/// extents of the segments as (extent, sequence).
fn source(path: &Path) -> Vec<(u64, u64)> {
    ContainerFile::create(path, SHIFT).expect("create container");
    let mut file = ContainerFile::open_unchecked(path).expect("open container");
    let id = file.add_collection("users", b"{}").expect("add").header.id;
    let mut segments = Vec::new();
    for number in 0..4 {
        let allocation = file.allocate(SEGMENT, id).expect("allocate");
        file.write_payload(allocation.index, 0, &payload(number))
            .expect("write");
        segments.push((allocation.index, allocation.sequence));
    }
    file.release(segments.remove(1).0).expect("release");
    file.checkpoint().expect("checkpoint");
    let snapshot = file
        .write_snapshot(id, ContainerExtentKind::Locations, 1, b"locations")
        .expect("snapshot");
    let (active, sequence) = segments[2];
    file.publish_generation(
        0,
        ContainerGenerationSlot {
            generation: 3,
            writing: false,
            active_extent: active,
            active_sequence: sequence,
            committed_length: payload(3).len() as u64,
            next_lsn: 9,
            record_count: 8,
            table_epoch: 1,
        },
    )
    .expect("publish");
    file.record_checkpoint(
        0,
        ContainerCheckpointSlot {
            checkpoint: 1,
            lsn: 8,
            extent: segments[1].0,
            extent_sequence: segments[1].1,
            offset: 16,
            locations: Some(snapshot),
            index: None,
            schemas: None,
            dictionary: None,
        },
    )
    .expect("record checkpoint");
    file.checkpoint().expect("checkpoint");
    segments
}

#[test]
fn a_sealed_image_keeps_every_extent_in_use_renumbered_and_no_free_one() {
    let scratch = scratch("vdbx-sealing");
    let path = scratch.0.join("source.vdbx");
    let segments = source(&path);
    let reader = ContainerFile::open_reader(&path).expect("open reader");
    assert!(!reader.allocator().free_extents().is_empty());
    let out = scratch.0.join("sealed.vdbx");
    let report = reader.write_sealed(&out, None).expect("seal");
    assert!(report.stored_bytes < report.image_bytes / 10, "{report:?}");
    assert!(reader.write_sealed(&out, None).is_err(), "an existing file");

    let sealed = ContainerFile::open_reader(&out).expect("open sealed");
    assert!(sealed.is_sealed() && !sealed.is_locked());
    assert!(sealed.allocator().free_extents().is_empty());
    // The directory, three segments, the snapshot and one map extent.
    assert_eq!(sealed.superblock().extent_count, 7);
    let collection = &sealed.collections()[0];
    let id = collection.header.id;
    let owned = sealed.allocator().owned(SEGMENT, id);
    assert_eq!(owned.len(), 3);
    for ((extent, sequence), (number, (_, original))) in
        owned.iter().zip([0, 2, 3].into_iter().zip(&segments))
    {
        assert_eq!(sequence, original, "a segment keeps its number");
        sealed
            .expect_extent(*extent, SEGMENT, id, *sequence)
            .expect("its header names its new place");
        let mut bytes = vec![0; payload(number).len()];
        sealed.read_payload(*extent, 0, &mut bytes).expect("read");
        assert_eq!(bytes, payload(number));
    }
    // The slots name the new places of what they named.
    let generation = collection.generation.expect("generation slot");
    assert_eq!(
        (generation.active_extent, generation.active_sequence),
        owned[2]
    );
    let checkpoint = collection.checkpoint.expect("checkpoint slot");
    assert_eq!((checkpoint.extent, checkpoint.extent_sequence), owned[1]);
    let snapshot: ContainerSnapshotRef = checkpoint.locations.expect("snapshot");
    assert_eq!(
        sealed
            .read_snapshot(id, ContainerExtentKind::Locations, 1, &snapshot, 1024)
            .expect("snapshot reads"),
        b"locations"
    );
    let verified = sealed.verify().expect("verify");
    assert!(verified.ok && verified.problems.is_empty(), "{verified:?}");
    // It is never written.
    assert!(matches!(
        ContainerFile::open_unchecked(&out),
        Err(ContainerError::Sealed)
    ));
    assert!(sealed.write_payload(owned[0].0, 0, b"x").is_err());
}

#[test]
fn an_encrypted_sealed_image_opens_only_with_its_key() {
    let scratch = scratch("vdbx-sealing-encrypted");
    let path = scratch.0.join("source.vdbx");
    source(&path);
    let out = scratch.0.join("sealed.vdbx");
    ContainerFile::open_reader(&path)
        .expect("open reader")
        .write_sealed(&out, Some(b"the sealing secret"))
        .expect("seal");
    let bytes = std::fs::read(&out).expect("read file");
    assert!(!bytes.windows(9).any(|window| window == b"segment 3"));
    assert!(!bytes.windows(5).any(|window| window == b"users"));

    let mut sealed = ContainerFile::open_reader(&out).expect("open sealed");
    assert!(sealed.is_encrypted() && sealed.is_locked());
    assert!(sealed.collections().is_empty());
    assert!(matches!(
        sealed.unlock(b"another secret"),
        Err(ContainerError::Cipher(ContainerCipherError::WrongKey))
    ));
    sealed.unlock(b"the sealing secret").expect("unlock");
    assert!(!sealed.is_locked());
    assert_eq!(sealed.collections()[0].header.name, "users");
    let id = sealed.collections()[0].header.id;
    let (extent, _) = sealed.allocator().owned(SEGMENT, id)[2];
    let mut read = vec![0; payload(3).len()];
    sealed.read_payload(extent, 0, &mut read).expect("read");
    assert_eq!(read, payload(3));
    assert!(sealed.verify().expect("verify").ok);
    // The records inside are plain: the blocks are what is sealed.
    assert!(sealed.cipher().is_none());
}
