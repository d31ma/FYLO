use super::{ContainerError, ContainerFile};
use fylo_format::{
    ContainerCheckpointSlot, ContainerExtentKind, ContainerFormatError, ContainerGenerationSlot,
    ContainerSuperblock,
};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

// 1 MiB extents keep the test files small.
const SHIFT: u8 = ContainerSuperblock::MIN_EXTENT_SHIFT;
const EXTENT_BYTES: u64 = 1 << SHIFT;
const SEGMENT: ContainerExtentKind = ContainerExtentKind::Segment;

struct Database(PathBuf);

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn database(name: &str) -> (Database, PathBuf) {
    let dir = std::env::temp_dir().join(crate::write::unique_name(name));
    std::fs::create_dir_all(&dir).expect("create directory");
    let path = dir.join("test.fxdb");
    (Database(dir), path)
}

fn created(name: &str) -> (Database, PathBuf) {
    let (guard, path) = database(name);
    ContainerFile::create(&path, SHIFT).expect("create container");
    (guard, path)
}

fn generation(generation: u64, committed_length: u64) -> ContainerGenerationSlot {
    ContainerGenerationSlot {
        generation,
        writing: false,
        active_extent: 3,
        active_sequence: 3,
        committed_length,
        next_lsn: generation + 1,
        record_count: generation,
        table_epoch: 0,
    }
}

fn checkpoint(checkpoint: u64) -> ContainerCheckpointSlot {
    ContainerCheckpointSlot {
        checkpoint,
        lsn: checkpoint * 10,
        extent: 3,
        extent_sequence: 3,
        offset: 512,
        locations: None,
        index: None,
        dictionary: None,
    }
}

fn file_length(path: &Path) -> u64 {
    std::fs::metadata(path).expect("stat container").len()
}

#[test]
fn a_new_container_opens_empty() {
    let (_guard, path) = created("fxdb-new");
    assert_eq!(file_length(&path), 3 * EXTENT_BYTES);
    let container = ContainerFile::open_unchecked(&path).expect("open container");
    let superblock = container.superblock();
    assert_eq!(superblock.generation, 1);
    assert_eq!(superblock.extent_shift, SHIFT);
    assert_eq!(superblock.extent_count, 3);
    assert_eq!(
        (superblock.directory_extent, superblock.map_first_extent),
        (1, 2)
    );
    assert!(container.collections().is_empty());
    assert!(container.allocator().free_extents().is_empty());
    container
        .expect_extent(1, ContainerExtentKind::Directory, 0, 1)
        .expect("directory header");
    container
        .expect_extent(2, ContainerExtentKind::Map, 0, 2)
        .expect("map header");
}

#[test]
fn create_refuses_an_existing_file_and_a_bad_extent_shift() {
    let (_guard, path) = created("fxdb-create-twice");
    assert!(matches!(
        ContainerFile::create(&path, SHIFT),
        Err(ContainerError::Io(error)) if error.kind() == ErrorKind::AlreadyExists
    ));
    let (_other, fresh) = database("fxdb-bad-shift");
    for shift in [
        ContainerSuperblock::MIN_EXTENT_SHIFT - 1,
        ContainerSuperblock::MAX_EXTENT_SHIFT + 1,
    ] {
        assert!(matches!(
            ContainerFile::create(&fresh, shift),
            Err(ContainerError::Format(ContainerFormatError::InvalidField))
        ));
    }
    assert!(!fresh.exists());
}

#[test]
fn a_file_that_is_not_a_container_is_refused() {
    let (_guard, path) = database("fxdb-garbage");
    std::fs::write(&path, vec![0x5a_u8; 16 * 1024]).expect("write garbage");
    assert!(matches!(
        ContainerFile::open_unchecked(&path),
        Err(ContainerError::Format(ContainerFormatError::BadMagic))
    ));
    std::fs::write(&path, b"short").expect("write short file");
    assert!(matches!(
        ContainerFile::open_unchecked(&path),
        Err(ContainerError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
    ));
}

#[test]
fn collections_and_their_slots_survive_reopening() {
    let (_guard, path) = created("fxdb-collections");
    {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        let users = container
            .add_collection("users", b"{\"schema\":1}")
            .expect("add users")
            .block;
        let orders = container
            .add_collection("orders", b"")
            .expect("add orders")
            .block;
        assert_eq!((users, orders), (0, 1));
        assert!(matches!(
            container.add_collection("users", b""),
            Err(ContainerError::CollectionExists)
        ));
        for step in 1..=3 {
            container
                .publish_generation(users, generation(step, step * 100))
                .expect("publish generation");
        }
        container
            .record_checkpoint(users, checkpoint(1))
            .expect("record checkpoint");
        container
            .record_checkpoint(users, checkpoint(2))
            .expect("record checkpoint");
        container.sync().expect("sync");
        assert!(container.publish_generation(9, generation(1, 0)).is_err());
    }
    let container = ContainerFile::open_unchecked(&path).expect("reopen container");
    let collections = container.collections();
    assert_eq!(collections.len(), 2);
    assert_eq!(collections[0].header.name, "users");
    assert_eq!(collections[0].header.id, 1);
    assert_eq!(collections[0].header.descriptor, b"{\"schema\":1}");
    assert_eq!(collections[0].generation, Some(generation(3, 300)));
    assert_eq!(collections[0].checkpoint, Some(checkpoint(2)));
    assert_eq!(collections[1].header.name, "orders");
    assert_eq!(collections[1].header.id, 2);
    assert_eq!(collections[1].generation, None);
    assert_eq!(collections[1].checkpoint, None);
}

#[test]
fn a_torn_generation_slot_falls_back_to_the_other() {
    let (_guard, path) = created("fxdb-torn-slot");
    let block = {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        let block = container.add_collection("users", b"").expect("add").block;
        container
            .publish_generation(block, generation(1, 10))
            .expect("publish");
        container
            .publish_generation(block, generation(2, 20))
            .expect("publish");
        container.sync().expect("sync");
        block
    };
    // Generation 2 went into slot B; tear it.
    let slot_b = EXTENT_BYTES
        + 4096
        + u64::from(block) * 4096
        + super::ContainerCollectionHeader::GENERATION_SLOTS_OFFSET as u64
        + ContainerGenerationSlot::ENCODED_BYTES as u64;
    let mut bytes = std::fs::read(&path).expect("read container");
    let at = usize::try_from(slot_b).expect("offset") + 9;
    bytes[at] ^= 0xff;
    std::fs::write(&path, &bytes).expect("tear slot");
    let container = ContainerFile::open_unchecked(&path).expect("reopen container");
    assert_eq!(
        container.collections()[0].generation,
        Some(generation(1, 10))
    );
}

#[test]
fn payloads_stay_inside_their_extent() {
    let (_guard, path) = created("fxdb-payload");
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    let segment = container.allocate(SEGMENT, 1).expect("allocate");
    assert_eq!((segment.index, segment.grows_file), (3, true));
    assert_eq!(file_length(&path), 4 * EXTENT_BYTES);
    let capacity = container.payload_capacity();
    container
        .write_payload(segment.index, capacity - 5, b"tail!")
        .expect("write at the end of the payload");
    let mut read = [0_u8; 5];
    container
        .read_payload(segment.index, capacity - 5, &mut read)
        .expect("read back");
    assert_eq!(&read, b"tail!");
    for (extent, offset) in [(0, 0), (segment.index, capacity - 4), (4, 0)] {
        assert_eq!(
            container
                .write_payload(extent, offset, b"tail!")
                .expect_err("outside the extent")
                .kind(),
            ErrorKind::InvalidInput
        );
    }
    container
        .expect_extent(segment.index, SEGMENT, 1, segment.sequence)
        .expect("segment header");
    assert!(matches!(
        container.expect_extent(segment.index, SEGMENT, 1, segment.sequence + 1),
        Err(ContainerError::Format(_))
    ));
}

// Found by the `container_file` fuzz target: an extent index read from a
// damaged location overflowed the header offset.
#[test]
fn an_extent_beyond_any_file_is_refused_without_overflow() {
    let (_guard, path) = created("fxdb-extent-overflow");
    let container = ContainerFile::open_reader(&path).expect("open container");
    for extent in [u64::MAX, (u64::MAX >> SHIFT) + 1] {
        assert!(matches!(
            container.expect_extent(extent, SEGMENT, 1, 1),
            Err(ContainerError::Io(error)) if error.kind() == ErrorKind::InvalidInput
        ));
    }
    let past_the_map = container.allocator().extent_count();
    assert!(matches!(
        container.expect_extent(past_the_map, SEGMENT, 1, 1),
        Err(ContainerError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
    ));
}

#[test]
fn a_checkpoint_persists_the_extent_map() {
    let (_guard, path) = created("fxdb-checkpoint");
    let (first, second) = {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        let first = container.allocate(SEGMENT, 1).expect("allocate");
        let second = container.allocate(SEGMENT, 1).expect("allocate");
        container.seal(first.index, 4096).expect("seal");
        container.sync().expect("sync");
        container.checkpoint().expect("checkpoint");
        let superblock = container.superblock();
        assert_eq!(superblock.generation, 2);
        // The old map at extent 2 is released, so the new one goes to the end.
        assert_eq!(
            (superblock.map_first_extent, superblock.map_extent_count),
            (5, 1)
        );
        (first, second)
    };
    let mut container = ContainerFile::open_unchecked(&path).expect("reopen container");
    let superblock = container.superblock().clone();
    assert_eq!(superblock.generation, 2);
    assert_eq!(superblock.extent_count, 6);
    assert_eq!(superblock.next_extent_sequence, 6);
    assert_eq!(container.allocator().free_extents(), vec![2]);
    let plan = container.allocator().map_pages();
    assert_eq!(plan[0].entries.len(), 5);
    container
        .expect_extent(first.index, SEGMENT, 1, first.sequence)
        .expect("first segment");
    container
        .expect_extent(second.index, SEGMENT, 1, second.sequence)
        .expect("second segment");
    // The extent the old map held is reused now that a map shows it free.
    let reused = container.allocate(SEGMENT, 2).expect("allocate");
    assert_eq!((reused.index, reused.grows_file), (2, false));
}

#[test]
fn a_released_extent_is_reused_only_after_a_checkpoint() {
    let (_guard, path) = created("fxdb-release");
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    let replaced = container.allocate(SEGMENT, 1).expect("allocate");
    container.seal(replaced.index, 0).expect("seal");
    container.release(replaced.index).expect("release");
    assert_eq!(container.allocate(SEGMENT, 1).expect("allocate").index, 4);
    container.checkpoint().expect("checkpoint");
    // Extents 2 (old map) and 3 (released) are free; the new map took 5.
    assert_eq!(container.allocator().free_extents(), vec![2, 3]);
    assert_eq!(container.allocate(SEGMENT, 1).expect("allocate").index, 2);
}

#[test]
fn a_torn_superblock_falls_back_to_the_older_copy() {
    let (_guard, path) = created("fxdb-torn-superblock");
    {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        container.allocate(SEGMENT, 1).expect("allocate");
        container
            .checkpoint()
            .expect("first checkpoint, into copy B");
        container.allocate(SEGMENT, 1).expect("allocate");
        container
            .checkpoint()
            .expect("second checkpoint, into copy A");
        assert_eq!(container.superblock().generation, 3);
    }
    let mut bytes = std::fs::read(&path).expect("read container");
    bytes[100] ^= 0xff;
    std::fs::write(&path, &bytes).expect("tear copy A");
    let container = ContainerFile::open_unchecked(&path).expect("reopen container");
    let superblock = container.superblock();
    // Copy B still names the map it was written with, which the second
    // checkpoint released but did not overwrite.
    assert_eq!(superblock.generation, 2);
    assert_eq!(superblock.extent_count, 5);
    assert_eq!(superblock.map_first_extent, 4);
    assert_eq!(container.allocator().free_extents(), vec![2]);
}

#[test]
fn checkpoints_alternate_between_the_superblock_copies() {
    let (_guard, path) = created("fxdb-alternate");
    let mut container = ContainerFile::open_unchecked(&path).expect("open container");
    for expected in 2..=6 {
        container.checkpoint().expect("checkpoint");
        assert_eq!(container.superblock().generation, expected);
    }
    // Two map extents alternate once the first checkpoint has run.
    assert_eq!(container.superblock().extent_count, 4);
    drop(container);
    let bytes = std::fs::read(&path).expect("read container");
    let size = ContainerSuperblock::ENCODED_BYTES;
    let (newest, in_a) =
        ContainerSuperblock::newest(&bytes[..size], &bytes[size..2 * size]).expect("superblocks");
    assert_eq!((newest.generation, in_a), (6, false));
}

fn released_tail(path: &Path) -> ContainerFile {
    let mut container = ContainerFile::open_unchecked(path).expect("open container");
    let kept = container.allocate(SEGMENT, 1).expect("allocate");
    container
        .write_payload(kept.index, 0, b"kept")
        .expect("write kept");
    for _ in 0..3 {
        let replaced = container.allocate(SEGMENT, 1).expect("allocate");
        container
            .write_payload(replaced.index, 0, &vec![7_u8; 256 * 1024])
            .expect("write replaced");
        container.seal(replaced.index, 0).expect("seal");
        container.release(replaced.index).expect("release");
    }
    container.sync().expect("sync");
    container.checkpoint().expect("checkpoint");
    container
}

#[test]
fn shrinking_cuts_free_extents_off_the_end() {
    let (_guard, path) = created("fxdb-shrink");
    let mut container = released_tail(&path);
    // Extents 4 to 6 are free, but the new map sits at 7, after them.
    assert_eq!(container.allocator().free_extents(), vec![2, 4, 5, 6]);
    assert_eq!(container.shrink_free_tail().expect("shrink"), 0);
    // The next checkpoint moves the map to 2, which frees the tail.
    container.checkpoint().expect("checkpoint");
    assert_eq!(container.superblock().map_first_extent, 2);
    assert_eq!(file_length(&path), 8 * EXTENT_BYTES);
    assert_eq!(
        container.shrink_free_tail().expect("shrink"),
        3 * EXTENT_BYTES
    );
    assert_eq!(file_length(&path), 5 * EXTENT_BYTES);
    drop(container);
    let container = ContainerFile::open_unchecked(&path).expect("reopen container");
    assert_eq!(container.superblock().extent_count, 5);
    let mut kept = [0_u8; 4];
    container
        .read_payload(3, 0, &mut kept)
        .expect("read kept payload");
    assert_eq!(&kept, b"kept");
}

#[test]
fn an_older_superblock_that_outlives_a_shrink_still_opens() {
    let (_guard, path) = created("fxdb-shrink-fallback");
    {
        let mut container = released_tail(&path);
        container.checkpoint().expect("checkpoint");
        container.shrink_free_tail().expect("shrink");
    }
    // Tear the newest copy; the older one describes eight extents.
    let mut bytes = std::fs::read(&path).expect("read container");
    let newest_at = {
        let size = ContainerSuperblock::ENCODED_BYTES;
        let (_, in_a) = ContainerSuperblock::newest(&bytes[..size], &bytes[size..2 * size])
            .expect("superblocks");
        if in_a { 0 } else { size }
    };
    bytes[newest_at + 100] ^= 0xff;
    std::fs::write(&path, &bytes).expect("tear newest copy");
    let container = ContainerFile::open_unchecked(&path).expect("open on the older copy");
    assert_eq!(container.superblock().extent_count, 8);
    assert_eq!(file_length(&path), 8 * EXTENT_BYTES);
}

/// The bytes the file has allocated on disk, which a punched hole frees.
#[cfg(unix)]
fn allocated_bytes(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).expect("stat").blocks() * 512
}

#[cfg(windows)]
fn allocated_bytes(path: &Path) -> u64 {
    super::punch::allocated_bytes(path).expect("allocated size")
}

#[cfg(any(unix, windows))]
#[test]
fn punching_returns_free_extents_space_where_supported() {
    let (_guard, path) = created("fxdb-punch");
    let container = released_tail(&path);
    let before = allocated_bytes(&path);
    let punched = container.punch_free_extents().expect("punch");
    let after = allocated_bytes(&path);
    assert_eq!(file_length(&path), 8 * EXTENT_BYTES);
    if punched > 0 {
        assert_eq!(punched, 4);
        assert!(after < before, "{after} blocks after, {before} before");
    }
    let mut kept = [0_u8; 4];
    container.read_payload(3, 0, &mut kept).expect("read kept");
    assert_eq!(&kept, b"kept");
}

#[test]
fn the_report_says_plainly_that_encryption_is_off() {
    let (_guard, path) = created("fxdb-report");
    let mut container = released_tail(&path);
    let users = container.add_collection("users", b"").expect("add").block;
    container
        .publish_generation(users, generation(4, 400))
        .expect("publish");
    let report = container.report().expect("report");
    assert!(!report.encrypted);
    assert!(!report.sealed);
    assert_eq!(report.extent_count, 8);
    assert_eq!(report.file_bytes, 8 * EXTENT_BYTES);
    assert_eq!(report.extents.free, 4);
    assert_eq!(report.extents.by_kind["segment"].extents, 1);
    assert_eq!(report.extents.by_kind["map"].extents, 1);
    assert_eq!(report.collections[0].name, "users");
    assert_eq!(report.collections[0].records, 4);
    let json = serde_json::to_value(&report).expect("serialize");
    assert_eq!(json["encrypted"], serde_json::Value::Bool(false));
    assert_eq!(json["extents"]["byKind"]["directory"]["extents"], 1);
    assert_eq!(
        json["collections"][0]["checkpointLsn"],
        serde_json::Value::Null
    );
}

#[test]
fn a_reader_reads_a_read_only_file_and_cannot_write() {
    let (_guard, path) = created("fxdb-reader");
    {
        let mut container = ContainerFile::open_unchecked(&path).expect("open container");
        container.add_collection("users", b"").expect("add");
        let segment = container.allocate(SEGMENT, 1).expect("allocate");
        container
            .write_payload(segment.index, 0, b"hello")
            .expect("write");
        container.checkpoint().expect("checkpoint");
    }
    let mut permissions = std::fs::metadata(&path).expect("stat").permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).expect("make read-only");
    let mut reader = ContainerFile::open_reader(&path).expect("open to read");
    assert!(!reader.is_writable());
    assert_eq!(reader.collections()[0].header.name, "users");
    let mut hello = [0_u8; 5];
    reader.read_payload(3, 0, &mut hello).expect("read");
    assert_eq!(&hello, b"hello");
    assert!(matches!(
        reader.allocate(SEGMENT, 1),
        Err(ContainerError::ReadOnly)
    ));
    assert!(matches!(reader.checkpoint(), Err(ContainerError::ReadOnly)));
    assert!(matches!(
        reader.add_collection("orders", b""),
        Err(ContainerError::ReadOnly)
    ));
    assert_eq!(
        reader
            .write_payload(3, 0, b"x")
            .expect_err("read-only")
            .kind(),
        ErrorKind::PermissionDenied
    );
}

#[test]
fn a_reader_opens_a_file_a_writer_may_not() {
    let (_guard, path) = created("fxdb-reader-ro-compat");
    let mut bytes = std::fs::read(&path).expect("read container");
    let size = ContainerSuperblock::ENCODED_BYTES;
    let mut superblock = ContainerSuperblock::decode(&bytes[..size]).expect("decode");
    superblock.read_only_compat = ContainerSuperblock::READ_ONLY_COMPAT_SCHEMAS;
    bytes[..size].copy_from_slice(&superblock.encode());
    std::fs::write(&path, &bytes).expect("write flag");
    assert!(matches!(
        ContainerFile::open_unchecked(&path),
        Err(ContainerError::ReadOnly)
    ));
    ContainerFile::open_reader(&path).expect("a reader still opens it");
}

const SECRET: &[u8] = b"correct horse battery staple";

fn encrypted(name: &str) -> (Database, PathBuf) {
    let (guard, path) = database(name);
    ContainerFile::create_encrypted(&path, SHIFT, SECRET).expect("create encrypted container");
    (guard, path)
}

#[test]
fn an_encrypted_file_lists_nothing_until_unlocked() {
    let (_guard, path) = encrypted("fxdb-encrypted-locked");
    {
        let mut writer = ContainerFile::open_unchecked(&path).unwrap();
        assert!(writer.is_encrypted() && writer.is_locked());
        let refused = writer.add_collection("users", b"{}").unwrap_err();
        assert!(matches!(refused, ContainerError::Locked));
        assert_eq!(refused.code(), "EKEY");
        writer.unlock(SECRET).unwrap();
        writer
            .add_collection("users", b"{\"kind\":\"document\"}")
            .unwrap();
        writer.add_collection("orders", b"{}").unwrap();
    }
    let mut reader = ContainerFile::open_reader(&path).unwrap();
    assert!(reader.collections().is_empty());
    let report = reader.report().unwrap();
    assert!(report.encrypted && report.locked && report.collections.is_empty());

    let wrong = reader.unlock(b"not the secret").unwrap_err();
    assert_eq!(wrong.code(), "EKEY");
    assert!(reader.is_locked());

    reader.unlock(SECRET).unwrap();
    let names: Vec<_> = reader
        .collections()
        .iter()
        .map(|collection| collection.header.name.as_str())
        .collect();
    assert_eq!(names, ["users", "orders"]);
    assert_eq!(
        reader.collections()[0].header.descriptor,
        b"{\"kind\":\"document\"}"
    );
    assert!(!reader.report().unwrap().locked);
}

#[test]
fn an_encrypted_directory_hides_collection_names() {
    let (_guard, path) = encrypted("fxdb-encrypted-names");
    let mut writer = ContainerFile::open_unchecked(&path).unwrap();
    writer.unlock(SECRET).unwrap();
    writer
        .add_collection("payroll-salaries", b"{\"secret\":\"descriptor\"}")
        .unwrap();
    drop(writer);
    let bytes = std::fs::read(&path).unwrap();
    for needle in [&b"payroll-salaries"[..], b"descriptor"] {
        assert!(!bytes.windows(needle.len()).any(|window| window == needle));
    }
}

#[test]
fn a_sealed_collection_block_moved_to_another_block_is_refused() {
    let (_guard, path) = encrypted("fxdb-encrypted-moved");
    let directory = {
        let mut writer = ContainerFile::open_unchecked(&path).unwrap();
        writer.unlock(SECRET).unwrap();
        writer.add_collection("first", b"{}").unwrap();
        writer.add_collection("second", b"{}").unwrap();
        writer.superblock().directory_extent
    };
    // Copy block 0 over block 1: its checksum still holds, but it was sealed
    // for block 0.
    let block_at = |block: u64| directory * EXTENT_BYTES + 4096 + block * 4096;
    let mut bytes = std::fs::read(&path).unwrap();
    let first = usize::try_from(block_at(0)).unwrap();
    let second = usize::try_from(block_at(1)).unwrap();
    bytes.copy_within(first..first + 4096, second);
    std::fs::write(&path, bytes).unwrap();

    let mut reader = ContainerFile::open_reader(&path).unwrap();
    let refused = reader.unlock(SECRET).unwrap_err();
    assert_eq!(refused.code(), "EFXDB_CORRUPT");
    assert!(reader.is_locked());
}

#[test]
fn unlocking_a_plain_file_does_nothing() {
    let (_guard, path) = created("fxdb-plain-unlock");
    let mut writer = ContainerFile::open_unchecked(&path).unwrap();
    assert!(!writer.is_encrypted() && !writer.is_locked());
    writer.unlock(b"anything").unwrap();
    assert!(writer.cipher().is_none());
    writer.add_collection("users", b"{}").unwrap();
    assert!(!writer.report().unwrap().locked);
}

#[test]
fn a_reused_segment_extent_holds_none_of_its_former_bytes() {
    let (_guard, path) = created("fxdb-reuse-clears");
    let mut container = ContainerFile::open_unchecked(&path).unwrap();
    let first = container.allocate(SEGMENT, 1).unwrap();
    container
        .write_payload(first.index, 0, &vec![0xAB; 64 * 1024])
        .unwrap();
    assert!(!container.allocator().is_mapped(first.index));
    container.checkpoint().unwrap();
    assert!(container.allocator().is_mapped(first.index));
    container.release(first.index).unwrap();
    container.checkpoint().unwrap();

    let second = container.allocate(SEGMENT, 1).unwrap();
    assert_eq!(second.index, first.index);
    assert_eq!(
        container.allocator().find_sequence(second.sequence),
        Some((second.index, SEGMENT, 1))
    );
    assert_eq!(container.allocator().find_sequence(first.sequence), None);
    assert_eq!(
        container.allocator().owned(SEGMENT, 1),
        [(second.index, second.sequence)]
    );
    let mut payload = vec![0xFF_u8; 64 * 1024];
    container
        .read_payload(second.index, 0, &mut payload)
        .unwrap();
    assert!(payload.iter().all(|byte| *byte == 0));
}

#[test]
fn an_extent_only_a_slot_names_is_reclaimed_after_a_reopen() {
    let (_guard, path) = created("fxdb-reclaim");
    let mut container = ContainerFile::open_unchecked(&path).unwrap();
    let block = container.add_collection("users", b"{}").unwrap().block;
    let active = container.allocate(SEGMENT, 1).unwrap();
    let slot = ContainerGenerationSlot {
        active_extent: active.index,
        active_sequence: active.sequence,
        ..generation(2, 0)
    };
    container.publish_generation(block, slot).unwrap();
    container.sync().unwrap();
    drop(container);

    let mut reopened = ContainerFile::open_unchecked(&path).unwrap();
    assert_eq!(reopened.read_generation(block).unwrap(), Some(slot));
    assert_eq!(reopened.read_checkpoint(block).unwrap(), None);
    assert_eq!(reopened.allocator().find_sequence(active.sequence), None);
    assert!(matches!(
        reopened.reclaim(active.index, SEGMENT, 1, active.sequence + 1),
        Err(ContainerError::Format(ContainerFormatError::InvalidField))
    ));
    reopened
        .reclaim(active.index, SEGMENT, 1, active.sequence)
        .unwrap();
    assert_eq!(
        reopened.allocator().find_sequence(active.sequence),
        Some((active.index, SEGMENT, 1))
    );
    assert!(!reopened.allocator().is_mapped(active.index));
    let next = reopened.allocate(SEGMENT, 1).unwrap();
    assert_ne!(next.index, active.index);
    assert!(next.sequence > active.sequence);
}

#[test]
fn a_reader_refreshes_its_map_after_a_writer_checkpoints() {
    let (_guard, path) = created("fxdb-refresh");
    let mut writer = ContainerFile::open_unchecked(&path).unwrap();
    let block = writer.add_collection("users", b"{}").unwrap().block;
    let mut reader = ContainerFile::open_reader(&path).unwrap();
    assert!(!reader.refresh().unwrap(), "nothing changed yet");

    let first = writer.allocate(SEGMENT, 1).unwrap();
    writer.checkpoint().unwrap();
    let active = writer.allocate(SEGMENT, 1).unwrap();
    let slot = ContainerGenerationSlot {
        active_extent: active.index,
        active_sequence: active.sequence,
        ..generation(2, 0)
    };
    writer.publish_generation(block, slot).unwrap();
    writer.sync().unwrap();
    assert!(!writer.refresh().unwrap(), "a writer's map is the newest");

    assert_eq!(reader.allocator().find_sequence(first.sequence), None);
    assert!(reader.refresh().unwrap());
    assert_eq!(
        reader.allocator().find_sequence(first.sequence),
        Some((first.index, SEGMENT, 1))
    );
    assert_eq!(reader.collections().len(), 1);
    assert!(!reader.refresh().unwrap());

    // The active extent is in no map on disk yet; the reader takes it back
    // in memory from the slot that names it, and can read it.
    assert_eq!(reader.allocator().find_sequence(active.sequence), None);
    assert_eq!(reader.read_generation(block).unwrap(), Some(slot));
    reader
        .reclaim(active.index, SEGMENT, 1, active.sequence)
        .unwrap();
    let mut payload = [0_u8; 16];
    reader.read_payload(active.index, 0, &mut payload).unwrap();
    assert!(matches!(
        reader.allocate(SEGMENT, 1),
        Err(ContainerError::ReadOnly)
    ));
}

#[test]
fn a_dropped_collection_keeps_its_block_and_gives_its_extents_back() {
    let (_guard, path) = created("fxdb-drop");
    let (users, segment) = {
        let mut container = ContainerFile::open_unchecked(&path).unwrap();
        let users = container.add_collection("users", b"{}").unwrap().header.id;
        container.add_collection("orders", b"{}").unwrap();
        let segment = container.allocate(SEGMENT, users).unwrap().index;
        container.checkpoint().unwrap();
        container.publish_generation(0, generation(1, 64)).unwrap();
        assert_eq!(container.drop_collection("users").unwrap(), 0);
        assert!(container.drop_collection("users").is_err());
        assert!(container.allocator().owned(SEGMENT, users).is_empty());
        container.checkpoint().unwrap();
        (users, segment)
    };
    let mut container = ContainerFile::open_unchecked(&path).unwrap();
    let blocks: Vec<_> = container
        .collections()
        .iter()
        .map(|collection| (collection.header.name.as_str(), collection.header.dropped))
        .collect();
    assert_eq!(blocks, [("users", true), ("orders", false)]);
    assert_eq!(
        container.collections()[0].generation,
        Some(generation(1, 64))
    );
    let again = container.add_collection("users", b"{}").unwrap();
    assert_eq!((again.block, again.header.id), (2, 3));
    assert_ne!(again.header.id, users);
    assert_eq!(container.allocate(SEGMENT, 3).unwrap().index, segment);
}

#[test]
fn a_writer_refreshes_to_the_map_another_writer_checkpointed() {
    let (_guard, path) = created("fxdb-writers-refresh");
    let mut first = ContainerFile::open_unchecked(&path).unwrap();
    first.add_collection("users", b"{}").unwrap();
    let mut second = ContainerFile::open_unchecked(&path).unwrap();
    let taken = first.allocate(SEGMENT, 1).unwrap();
    first.checkpoint().unwrap();

    // The second writer reads the first one's map before it allocates, as
    // it does under the allocation byte, and so never takes the same
    // extent or sequence number.
    assert_eq!(second.allocator().find_sequence(taken.sequence), None);
    assert!(second.refresh().unwrap());
    assert_eq!(
        second.allocator().find_sequence(taken.sequence),
        Some((taken.index, SEGMENT, 1))
    );
    let next = second.allocate(SEGMENT, 1).unwrap();
    assert_ne!(next.index, taken.index);
    assert!(next.sequence > taken.sequence);
    second.checkpoint().unwrap();
    assert!(first.refresh().unwrap());
    assert_eq!(
        first.allocator().find_sequence(next.sequence),
        Some((next.index, SEGMENT, 1))
    );
}
