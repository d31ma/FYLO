//! Large objects in `.vdbx` files (ADR 0034, slice L1-9a): extent runs and
//! chunks, streaming and range reads, the crash order, compaction, and the
//! rule that a key is never used twice with the same nonce. Every key is
//! generated here, and every file lives in the temporary folder.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use byckr_format::{ContainerObjectLayout, ContainerSuperblock};
use byckr_storage_native::{
    ContainerFile, ContainerObject, ContainerObjectChunk, ContainerObjectError,
    ContainerObjectMover, ContainerObjectOrphans, ContainerObjectReader, ContainerObjectSealer,
    ContainerObjectTarget, ContainerObjectWriter,
};
use sha2::{Digest, Sha256};

const MIB: u64 = 1 << 20;
const SMALL_SHIFT: u8 = 20;
const DEFAULT_SHIFT: u8 = 21;
const FILE_KEY: [u8; 32] = [7; 32];
const CANARY: &[u8] = b"WOMBAT-OBJECT-CANARY";

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fylo-vdbx-objects-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join("photos.vdbx")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Database {
    _scratch: Scratch,
    path: PathBuf,
    container: ContainerFile,
    collection: u32,
}

fn database(label: &str, shift: u8) -> Database {
    let scratch = Scratch::new(label);
    let path = scratch.file();
    ContainerFile::create(&path, shift).unwrap();
    let mut container = ContainerFile::open_unchecked(&path).unwrap();
    let collection = container.add_collection("photos", b"").unwrap().header.id;
    Database {
        _scratch: scratch,
        path,
        container,
        collection,
    }
}

impl Database {
    fn target(&self, id: &str) -> ContainerObjectTarget {
        ContainerObjectTarget {
            collection: self.collection,
            id: id.to_owned(),
        }
    }

    fn put(&mut self, id: &str, bytes: &[u8], key: Option<&[u8; 32]>) -> ContainerObject {
        let target = self.target(id);
        ContainerObjectWriter::begin(&mut self.container, target, bytes.len() as u64, key)
            .unwrap()
            .copy_from(&mut &bytes[..])
            .unwrap()
    }

    fn get(
        &self,
        object: &ContainerObject,
        key: Option<&[u8; 32]>,
    ) -> Result<Vec<u8>, ContainerObjectError> {
        let mut bytes = Vec::new();
        ContainerObjectReader::open(&self.container, object, key)?
            .read_to_end(&mut bytes)
            .map_err(ContainerObjectError::from_read)?;
        Ok(bytes)
    }

    fn get_range(
        &self,
        object: &ContainerObject,
        key: Option<&[u8; 32]>,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, ContainerObjectError> {
        let mut bytes = Vec::new();
        ContainerObjectReader::open_range(&self.container, object, key, range)?
            .read_to_end(&mut bytes)
            .map_err(ContainerObjectError::from_read)?;
        Ok(bytes)
    }

    fn reopen(&mut self) {
        self.container = ContainerFile::open_unchecked(&self.path).unwrap();
    }

    fn layout(&self, encrypted: bool) -> ContainerObjectLayout {
        let tag = if encrypted { 16 } else { 0 };
        ContainerObjectLayout::for_extent(self.container.superblock().extent_bytes(), tag)
    }

    fn frame_offset(&self, object: &ContainerObject, index: u64) -> u64 {
        let layout = self.layout(object.salt.is_some());
        let place = layout.place(index);
        let extent = object.extent_at(place.ordinal).unwrap();
        extent * self.container.superblock().extent_bytes() + 4096 + place.offset
    }

    fn raw(&self, at: u64, length: usize) -> Vec<u8> {
        let mut file = File::open(&self.path).unwrap();
        file.seek(SeekFrom::Start(at)).unwrap();
        let mut bytes = vec![0_u8; length];
        file.read_exact(&mut bytes).unwrap();
        bytes
    }

    fn overwrite(&self, at: u64, bytes: &[u8]) {
        let mut file = OpenOptions::new().write(true).open(&self.path).unwrap();
        file.seek(SeekFrom::Start(at)).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    fn fill_until_reused(&mut self, id: &str, extent: u64) -> ContainerObject {
        for _ in 0..64 {
            let filler = self.put(id, b"x", None);
            if filler.extents().contains(&extent) {
                return filler;
            }
        }
        panic!("extent {extent} was never reused");
    }

    fn fill_free_below(&mut self, extent: u64) {
        while self
            .container
            .allocator()
            .free_extents()
            .first()
            .is_some_and(|free| *free < extent)
        {
            self.put("filler", b"x", None);
        }
    }

    fn slot_bytes(&self, encrypted: bool) -> usize {
        usize::try_from(self.layout(encrypted).slot_bytes).unwrap()
    }
}

fn pattern(length: u64, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn code(error: &ContainerObjectError) -> &'static str {
    error.code()
}

#[test]
fn round_trips_objects_of_every_boundary_size() {
    let mut db = database("sizes", DEFAULT_SHIFT);
    let chunk = db.layout(false).chunk_bytes;
    let sizes = [
        0,
        1,
        chunk - 1,
        chunk,
        chunk + 1,
        2 * chunk,
        2 * chunk + 1,
        5 * chunk + 3,
    ];
    for (seed, size) in sizes.into_iter().enumerate() {
        let bytes = pattern(size, seed as u64 + 1);
        let object = db.put(&format!("object-{seed}"), &bytes, None);
        assert_eq!(object.length, size);
        assert_eq!(object.chunk_size, chunk);
        assert_eq!(object.sha256, ContainerObject::hex(&Sha256::digest(&bytes)));
        assert_eq!(db.get(&object, None).unwrap(), bytes, "size {size}");
    }
}

#[test]
fn an_empty_object_takes_no_extent() {
    let mut db = database("empty", SMALL_SHIFT);
    let object = db.put("empty", b"", None);
    assert!(object.runs.is_empty());
    assert_eq!(db.get(&object, None).unwrap(), b"");
}

#[test]
fn one_mebibyte_extents_hold_one_chunk_each() {
    let mut db = database("small-extents", SMALL_SHIFT);
    let chunk = db.layout(false).chunk_bytes;
    let bytes = pattern(3 * chunk + 10, 3);
    let object = db.put("three", &bytes, None);
    assert_eq!(object.extents().len(), 4);
    assert_eq!(object.runs.len(), 1);
    assert_eq!(db.get(&object, None).unwrap(), bytes);
}

#[test]
fn reads_every_range_on_and_next_to_chunk_and_extent_boundaries() {
    let mut db = database("ranges", DEFAULT_SHIFT);
    let chunk = db.layout(false).chunk_bytes;
    let length = 4 * chunk + 100;
    let bytes = pattern(length, 9);
    let object = db.put("ranges", &bytes, None);
    let mut edges = vec![0, 1, length - 1, length];
    for boundary in [chunk, 2 * chunk, 3 * chunk, 4 * chunk] {
        edges.extend([boundary - 1, boundary, boundary + 1]);
    }
    for start in &edges {
        for end in edges.iter().filter(|end| *end >= start) {
            let read = db.get_range(&object, None, *start..*end).unwrap();
            let expected = &bytes[usize::try_from(*start).unwrap()..usize::try_from(*end).unwrap()];
            assert_eq!(read, expected, "range {start}..{end}");
        }
    }
}

#[test]
fn a_range_read_reads_only_the_chunks_that_cover_it() {
    let mut db = database("range-only", DEFAULT_SHIFT);
    let chunk = db.layout(false).chunk_bytes;
    let bytes = pattern(3 * chunk, 4);
    let object = db.put("seek", &bytes, None);
    let first = db.frame_offset(&object, 0);
    db.overwrite(first + 40, b"damage");
    let middle = db
        .get_range(&object, None, chunk + 5..2 * chunk - 5)
        .unwrap();
    assert_eq!(
        middle,
        &bytes[usize::try_from(chunk + 5).unwrap()..usize::try_from(2 * chunk - 5).unwrap()]
    );
    let whole = db.get(&object, None).unwrap_err();
    assert!(matches!(whole, ContainerObjectError::Damaged(_)), "{whole}");
}

#[test]
fn refuses_a_range_outside_the_object() {
    let mut db = database("outside", SMALL_SHIFT);
    let object = db.put("short", b"abc", None);
    let error = db.get_range(&object, None, 2..4).unwrap_err();
    assert!(matches!(
        error,
        ContainerObjectError::RangeOutside { length: 3, .. }
    ));
    let error = db
        .get_range(&object, None, std::ops::Range { start: 3, end: 2 })
        .unwrap_err();
    assert!(matches!(error, ContainerObjectError::RangeOutside { .. }));
}

#[test]
fn a_whole_read_checks_the_sha256() {
    let mut db = database("sha", SMALL_SHIFT);
    let mut object = db.put("hash", b"some bytes", None);
    object.sha256 = ContainerObject::hex(&[0_u8; 32]);
    let error = db.get(&object, None).unwrap_err();
    assert!(matches!(error, ContainerObjectError::Damaged(_)), "{error}");
}

#[test]
fn refuses_an_object_over_the_limit_before_writing() {
    let mut db = database("limit", SMALL_SHIFT);
    let length_before = fs::metadata(&db.path).unwrap().len();
    let extents_before = db.container.allocator().extent_count();
    let target = db.target("huge");
    let error = ContainerObjectWriter::begin(
        &mut db.container,
        target,
        ContainerObject::MAX_BYTES + 1,
        None,
    )
    .err()
    .unwrap();
    assert!(matches!(error, ContainerObjectError::TooLarge { .. }));
    assert_eq!(code(&error), "ENATIVE_FILE_SIZE");
    assert_eq!(fs::metadata(&db.path).unwrap().len(), length_before);
    assert_eq!(db.container.allocator().extent_count(), extents_before);
    assert_eq!(
        db.container.superblock().incompat & ContainerSuperblock::INCOMPAT_LARGE_OBJECTS,
        0
    );
}

struct Pattern {
    left: u64,
    state: u64,
}

impl Read for Pattern {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = usize::try_from(self.left)
            .unwrap_or(usize::MAX)
            .min(out.len());
        for byte in &mut out[..count] {
            self.state = self
                .state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            *byte = self.state.to_le_bytes()[7];
        }
        self.left -= count as u64;
        Ok(count)
    }
}

struct HashSink(Sha256);

impl Write for HashSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn stream_through(label: &str, length: u64) {
    let mut db = database(label, DEFAULT_SHIFT);
    let target = db.target("video");
    let mut source = Pattern {
        left: length,
        state: 5,
    };
    let mut expected = HashSink(Sha256::new());
    let mut tee = Pattern {
        left: length,
        state: 5,
    };
    io::copy(&mut tee, &mut expected).unwrap();
    let object = ContainerObjectWriter::begin(&mut db.container, target, length, Some(&FILE_KEY))
        .unwrap()
        .copy_from(&mut source)
        .unwrap();
    assert_eq!(object.sha256, ContainerObject::hex(&expected.0.finalize()));
    let mut reader = ContainerObjectReader::open(&db.container, &object, Some(&FILE_KEY)).unwrap();
    let mut read = HashSink(Sha256::new());
    assert_eq!(io::copy(&mut reader, &mut read).unwrap(), length);
    assert_eq!(object.sha256, ContainerObject::hex(&read.0.finalize()));
}

#[test]
fn streams_a_large_encrypted_object() {
    stream_through("stream", 64 * MIB + 5);
}

#[test]
#[ignore = "writes and reads 512 MiB, about 5 minutes in a debug build: run with --run-ignored"]
fn streams_an_object_of_the_full_limit() {
    stream_through("full-limit", ContainerObject::MAX_BYTES);
}

#[test]
fn refuses_a_source_of_another_length_and_releases_its_extents() {
    let mut db = database("mismatch", SMALL_SHIFT);
    let target = db.target("short");
    let error = ContainerObjectWriter::begin(&mut db.container, target.clone(), 10, None)
        .unwrap()
        .copy_from(&mut &b"12345"[..])
        .unwrap_err();
    assert!(matches!(
        error,
        ContainerObjectError::LengthMismatch {
            declared: 10,
            written: 5
        }
    ));
    let error = ContainerObjectWriter::begin(&mut db.container, target, 3, None)
        .unwrap()
        .copy_from(&mut &b"12345"[..])
        .unwrap_err();
    assert!(
        matches!(
            error,
            ContainerObjectError::LengthMismatch { declared: 3, .. }
        ),
        "{error}"
    );
    assert!(
        ContainerObjectOrphans::find(&db.container, db.collection, &[])
            .extents
            .is_empty()
    );
}

#[test]
fn the_first_object_sets_the_large_objects_feature() {
    let mut db = database("feature", SMALL_SHIFT);
    assert_eq!(
        db.container.superblock().incompat & ContainerSuperblock::INCOMPAT_LARGE_OBJECTS,
        0
    );
    db.put("first", b"bytes", None);
    db.reopen();
    assert_ne!(
        db.container.superblock().incompat & ContainerSuperblock::INCOMPAT_LARGE_OBJECTS,
        0
    );
}

#[test]
fn a_crash_before_the_checkpoint_leaves_nothing_behind() {
    let mut db = database("crash-early", SMALL_SHIFT);
    let bytes = pattern(3 * MIB, 11);
    let lost = db.put("lost", &bytes, None);
    db.reopen();
    assert!(
        ContainerObjectOrphans::find(&db.container, db.collection, &[])
            .extents
            .is_empty()
    );
    let again = db.put("again", &bytes, None);
    assert_eq!(again.runs, lost.runs);
    assert_eq!(db.get(&again, None).unwrap(), bytes);
}

#[test]
fn a_crash_after_a_checkpoint_leaves_orphans_that_compaction_reclaims() {
    let mut db = database("crash-late", SMALL_SHIFT);
    let kept = db.put("kept", &pattern(MIB, 12), None);
    let target = db.target("orphan");
    let mut writer =
        ContainerObjectWriter::begin(&mut db.container, target, 3 * MIB, None).unwrap();
    writer.push(&pattern(MIB, 13)).unwrap();
    drop(writer);
    db.container.checkpoint().unwrap();
    db.reopen();
    let orphans =
        ContainerObjectOrphans::find(&db.container, db.collection, std::slice::from_ref(&kept));
    assert_eq!(orphans.extents.len(), 4);
    assert!(
        kept.extents()
            .iter()
            .all(|extent| !orphans.extents.contains(extent))
    );
    let released = orphans.extents.clone();
    assert_eq!(orphans.reclaim(&mut db.container).unwrap(), 4);
    db.container.checkpoint().unwrap();
    assert!(
        released
            .iter()
            .all(|extent| db.container.allocator().free_extents().contains(extent))
    );
    assert_eq!(db.get(&kept, None).unwrap(), pattern(MIB, 12));
}

#[test]
fn a_deleted_object_frees_its_extents_only_after_the_checkpoint() {
    let mut db = database("delete", SMALL_SHIFT);
    let old = db.put("old", &pattern(2 * MIB, 14), None);
    for extent in old.extents() {
        db.container.release(extent).unwrap();
    }
    let before = db.put("before", &pattern(MIB, 15), None);
    assert!(
        before
            .extents()
            .iter()
            .all(|extent| !old.extents().contains(extent))
    );
    db.container.checkpoint().unwrap();
    db.fill_until_reused("filler", old.extents()[0]);
    let error = db.get(&old, None).unwrap_err();
    assert!(matches!(error, ContainerObjectError::NotFound), "{error}");
}

#[test]
fn a_reader_of_the_same_record_id_never_reads_a_later_write() {
    let mut db = database("replace", SMALL_SHIFT);
    let first = db.put("same", &pattern(MIB, 17), None);
    for extent in first.extents() {
        db.container.release(extent).unwrap();
    }
    db.container.checkpoint().unwrap();
    let second = db.fill_until_reused("same", first.extents()[0]);
    assert_ne!(second.stamp, first.stamp);
    let error = db.get(&first, None).unwrap_err();
    assert!(matches!(error, ContainerObjectError::NotFound), "{error}");
}

#[test]
fn compaction_moves_objects_down_with_their_frames_unchanged() {
    let mut db = database("shrink", SMALL_SHIFT);
    let gone = db.put("gone", &pattern(3 * MIB, 19), None);
    let bytes = pattern(2 * MIB + 7, 20);
    let moved = db.put("moved", &bytes, Some(&FILE_KEY));
    let frame_before = db.raw(db.frame_offset(&moved, 1), db.slot_bytes(true));
    for extent in gone.extents() {
        db.container.release(extent).unwrap();
    }
    db.container.checkpoint().unwrap();
    let plan = ContainerObjectMover::relocate(&mut db.container, &moved)
        .unwrap()
        .unwrap();
    assert!(
        plan.moved_to
            .iter()
            .zip(&plan.moved_from)
            .all(|(to, from)| to < from)
    );
    let relocated = plan.commit(&mut db.container).unwrap();
    assert_eq!(relocated.salt, moved.salt);
    assert_eq!(relocated.stamp, moved.stamp);
    assert_eq!(
        db.raw(db.frame_offset(&relocated, 1), db.slot_bytes(true)),
        frame_before
    );
    assert_eq!(db.get(&relocated, Some(&FILE_KEY)).unwrap(), bytes);
    assert_eq!(db.get(&moved, Some(&FILE_KEY)).unwrap(), bytes);
    db.container.checkpoint().unwrap();
    db.fill_until_reused("filler", moved.extents()[0]);
    let error = db.get(&moved, Some(&FILE_KEY)).unwrap_err();
    assert!(matches!(error, ContainerObjectError::NotFound), "{error}");
    assert_eq!(db.get(&relocated, Some(&FILE_KEY)).unwrap(), bytes);
}

#[test]
fn compaction_leaves_an_object_with_no_lower_free_extent() {
    let mut db = database("no-move", SMALL_SHIFT);
    let object = db.put("front", &pattern(MIB, 22), None);
    db.fill_free_below(object.extents()[0]);
    assert!(
        ContainerObjectMover::relocate(&mut db.container, &object)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_cancelled_move_releases_the_copies() {
    let mut db = database("cancel", SMALL_SHIFT);
    let gone = db.put("gone", &pattern(MIB, 23), None);
    let moved = db.put("moved", &pattern(MIB, 24), None);
    for extent in gone.extents() {
        db.container.release(extent).unwrap();
    }
    db.container.checkpoint().unwrap();
    let plan = ContainerObjectMover::relocate(&mut db.container, &moved)
        .unwrap()
        .unwrap();
    let copies = plan.moved_to.clone();
    plan.cancel(&mut db.container).unwrap();
    db.container.checkpoint().unwrap();
    assert!(
        copies
            .iter()
            .all(|extent| db.container.allocator().free_extents().contains(extent))
    );
    assert_eq!(db.get(&moved, None).unwrap(), pattern(MIB, 24));
}

#[test]
fn encrypted_objects_round_trip_and_hide_their_bytes() {
    let mut db = database("encrypted", DEFAULT_SHIFT);
    let chunk = db.layout(true).chunk_bytes;
    let mut bytes = pattern(2 * chunk + 50, 25);
    bytes[..CANARY.len()].copy_from_slice(CANARY);
    let object = db.put("secret", &bytes, Some(&FILE_KEY));
    assert!(object.salt.is_some());
    assert_eq!(object.chunk_size, chunk);
    assert_eq!(db.get(&object, Some(&FILE_KEY)).unwrap(), bytes);
    let stored = fs::read(&db.path).unwrap();
    assert!(!stored.windows(CANARY.len()).any(|window| window == CANARY));
    let range = db
        .get_range(&object, Some(&FILE_KEY), chunk - 3..chunk + 3)
        .unwrap();
    assert_eq!(
        range,
        &bytes[usize::try_from(chunk - 3).unwrap()..usize::try_from(chunk + 3).unwrap()]
    );
}

#[test]
fn an_encrypted_object_needs_its_key() {
    let mut db = database("locked", SMALL_SHIFT);
    let object = db.put("secret", b"hidden", Some(&FILE_KEY));
    let error = db.get(&object, None).unwrap_err();
    assert_eq!(code(&error), "EKEY");
    let error = db.get(&object, Some(&[8; 32])).unwrap_err();
    assert!(matches!(error, ContainerObjectError::Damaged(_)), "{error}");
}

#[test]
fn a_moved_or_reordered_encrypted_chunk_fails_its_tag() {
    let mut db = database("reorder", DEFAULT_SHIFT);
    let chunk = db.layout(true).chunk_bytes;
    let bytes = pattern(2 * chunk, 26);
    let object = db.put("swap", &bytes, Some(&FILE_KEY));
    let slot = db.slot_bytes(true);
    let first = db.raw(db.frame_offset(&object, 0), slot);
    let second = db.raw(db.frame_offset(&object, 1), slot);
    db.overwrite(db.frame_offset(&object, 0), &second);
    db.overwrite(db.frame_offset(&object, 1), &first);
    let error = db.get(&object, Some(&FILE_KEY)).unwrap_err();
    assert!(matches!(error, ContainerObjectError::Damaged(_)), "{error}");
}

#[test]
fn an_encrypted_object_cannot_be_cut_short() {
    let mut db = database("cut", DEFAULT_SHIFT);
    let chunk = db.layout(true).chunk_bytes;
    let mut object = db.put("cut", &pattern(2 * chunk, 27), Some(&FILE_KEY));
    object.length = chunk;
    object.sha256 = ContainerObject::hex(&Sha256::digest(
        &pattern(2 * chunk, 27)[..usize::try_from(chunk).unwrap()],
    ));
    let error = db.get(&object, Some(&FILE_KEY)).unwrap_err();
    assert!(matches!(error, ContainerObjectError::Damaged(_)), "{error}");
}

#[test]
fn a_record_that_names_another_objects_extents_finds_nothing() {
    let mut db = database("transplant", SMALL_SHIFT);
    let a = db.put("a", &pattern(1000, 28), Some(&FILE_KEY));
    let b = db.put("b", &pattern(1000, 29), Some(&FILE_KEY));
    let mut forged = b.clone();
    forged.runs.clone_from(&a.runs);
    let error = db.get(&forged, Some(&FILE_KEY)).unwrap_err();
    assert!(matches!(error, ContainerObjectError::NotFound), "{error}");
}

#[test]
fn a_key_is_never_used_twice_with_the_same_nonce() {
    let target = ContainerObjectTarget {
        collection: 1,
        id: "nonce".to_owned(),
    };
    let chunk = |index| ContainerObjectChunk { index, count: 3 };
    let mut sealer = ContainerObjectSealer::new(&FILE_KEY).unwrap();
    let mut first = *b"chunk zero";
    sealer.seal_in_place(&target, chunk(0), &mut first).unwrap();
    let mut again = *b"chunk zero";
    let error = sealer
        .seal_in_place(&target, chunk(0), &mut again)
        .unwrap_err();
    assert!(matches!(
        error,
        ContainerObjectError::NonceReuse { index: 0 }
    ));
    let mut later = *b"chunk two!";
    sealer.seal_in_place(&target, chunk(2), &mut later).unwrap();
    let mut earlier = *b"chunk one!";
    let error = sealer
        .seal_in_place(&target, chunk(1), &mut earlier)
        .unwrap_err();
    assert!(matches!(
        error,
        ContainerObjectError::NonceReuse { index: 1 }
    ));
    let other = ContainerObjectSealer::new(&FILE_KEY).unwrap();
    assert_ne!(other.salt(), sealer.salt());
}

#[test]
fn every_object_write_gets_a_new_key() {
    let mut db = database("new-keys", SMALL_SHIFT);
    let bytes = pattern(2000, 30);
    let first = db.put("same", &bytes, Some(&FILE_KEY));
    let second = db.put("same", &bytes, Some(&FILE_KEY));
    assert_ne!(second.salt, first.salt);
    let first_frame = db.raw(db.frame_offset(&first, 0), 2000 + 48);
    let second_frame = db.raw(db.frame_offset(&second, 0), 2000 + 48);
    assert_ne!(first_frame[32..], second_frame[32..]);
    assert_eq!(db.get(&first, Some(&FILE_KEY)).unwrap(), bytes);
    assert_eq!(db.get(&second, Some(&FILE_KEY)).unwrap(), bytes);
}

#[test]
fn the_record_fields_round_trip_as_json() {
    let mut db = database("json", SMALL_SHIFT);
    let object = db.put("record", b"bytes", Some(&FILE_KEY));
    let json = serde_json::to_value(&object).unwrap();
    for field in [
        "collection",
        "id",
        "length",
        "sha256",
        "chunkSize",
        "runs",
        "salt",
        "stamp",
    ] {
        assert!(json.get(field).is_some(), "{field} in {json}");
    }
    let back: ContainerObject = serde_json::from_value(json).unwrap();
    assert_eq!(back, object);
    let plain = db.put("plain", b"bytes", None);
    assert!(serde_json::to_value(&plain).unwrap().get("salt").is_none());
}

#[test]
fn runs_join_neighbouring_extents() {
    assert_eq!(
        ContainerObject::runs_from(&[3, 4, 5, 9, 10, 2]),
        vec![[3, 3], [9, 2], [2, 1]]
    );
    let object = ContainerObject {
        target: ContainerObjectTarget {
            collection: 1,
            id: "runs".to_owned(),
        },
        length: 0,
        sha256: String::new(),
        chunk_size: 1,
        runs: vec![[3, 3], [9, 2]],
        salt: None,
        stamp: 0,
    };
    assert_eq!(object.extent_at(0), Some(3));
    assert_eq!(object.extent_at(2), Some(5));
    assert_eq!(object.extent_at(3), Some(9));
    assert_eq!(object.extent_at(5), None);
    assert_eq!(object.extents(), vec![3, 4, 5, 9, 10]);
}

#[test]
fn reads_stay_valid_after_a_reopen() {
    let mut db = database("reopen", DEFAULT_SHIFT);
    let bytes = pattern(3 * MIB, 31);
    let object = db.put("durable", &bytes, Some(&FILE_KEY));
    db.container.checkpoint().unwrap();
    db.reopen();
    assert_eq!(db.get(&object, Some(&FILE_KEY)).unwrap(), bytes);
}
